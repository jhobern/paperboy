use std::path::{Path, PathBuf};
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
use crate::hurl::{FormFieldKind, HurlEntry, RunStatus};
use crate::i18n::{Language, Status, Strings};
use crate::request::{self, AppVars, CaptureUpdate, build_request_json};

use super::editor::*;
use super::git_save::*;
use super::new_request::*;
use super::remote::*;
use super::wrapcache::TextPos;
use tui_panel_select::MultiSelectPanel;

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
    /// Picking a DESTINATION FOLDER for "Save Collection As" (including the
    /// Scratch Space, which has no file of its own). Confirms on `Space` like
    /// the Workspace folder pickers; a filename prompt (pre-filled with the
    /// chosen folder) follows, and the collection is written there.
    SaveCollectionChooseFolder,
    /// Picking a DESTINATION FOLDER for "Export Report CSV". Confirms like the
    /// other folder pickers (navigate to the folder, Tab to the filename
    /// editor, Enter writes `dir/<name>.csv`); the filename is seeded from the
    /// report's name. Replaces the old behaviour of silently writing the CSV
    /// into the process working directory.
    SaveReportCsvChooseFolder,
    /// Picking a DESTINATION FOLDER for "Save Report Baseline". Confirms like the
    /// other folder pickers; Enter writes `dir/<name>.baseline` (a JSON snapshot
    /// of the active report's last run, seeded from the report's name). The
    /// saved file is referenced by a `# baseline:` header directive so later
    /// runs diff against it (PaperTrail "Source B" comparison).
    SaveReportBaselineChooseFolder,
    /// Loading a PaperTrail `.trail` document from a local file into a new
    /// report tab (see [`crate::report::Report`]). Local-only, so — like
    /// `LoadRequest` — it skips the local-vs-git source step.
    OpenReport,
    /// Writing the active report tab's `.trail` source back to its own file
    /// (the "Save" destination); the target path is the report's remembered
    /// path (else a "Save As" folder pick precedes it).
    SaveReport,
    /// Picking a DESTINATION FOLDER for "Save Report As". Confirms like the
    /// other folder pickers; Enter writes `dir/<name>.report` (extension added
    /// when missing), seeded with the report's name.
    SaveReportChooseFolder,
    /// Picking a SOURCE FOLDER for a `FOR … IN FILES/FOLDERS` node in the
    /// structured report editor. Confirms on `Space` (the current directory,
    /// like `OpenWorkspace`); the chosen path is written into the loop's
    /// producer `dir`. The target node is parked in
    /// [`TuiApp::pending_node_folder`] (`FileAction` stays `Copy`, so the node
    /// path can't live in the variant).
    PickReportNodeFolder,
}

impl FileAction {
    /// Whether this browser action picks a destination FOLDER and then writes
    /// a file/folder named in the browser's inline filename editor (Tab to it,
    /// Enter to save). The "Save … As" folder pickers and the report-CSV
    /// export; not `OpenWorkspace` (which loads an existing folder) or the
    /// plain file pickers.
    pub(crate) fn is_save_to_folder(&self) -> bool {
        matches!(
            self,
            FileAction::SaveWorkspaceChooseFolder
                | FileAction::SaveCollectionChooseFolder
                | FileAction::SaveReportCsvChooseFolder
                | FileAction::SaveReportBaselineChooseFolder
                | FileAction::SaveReportChooseFolder
        )
    }
}

#[derive(Clone, PartialEq)]
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
    /// Naming a brand-new collection file being created inside a Workspace
    /// tab (the `usize` is the workspace collection index). The typed text is
    /// a path relative to the workspace root; subfolders are allowed and a
    /// `.hurl` extension is defaulted. Reached via `n` in the workspace
    /// destination picker.
    NewWorkspaceCollection(usize),
    /// Naming a brand-new `.trail` file being created inside a Workspace tab
    /// (the `usize` is the workspace collection index). Like
    /// [`Self::NewWorkspaceCollection`] the typed text is a path relative to
    /// the workspace root; subfolders are allowed and a `.trail` extension is
    /// defaulted. Reached via `R` in the workspace destination picker.
    NewWorkspaceReport(usize),
    /// Editing one report-flow node "as a line" in the structured node editor:
    /// the prompt is seeded with the node's single-line source form and, on
    /// commit, that text is re-parsed and swapped back into the flow at `path`
    /// (a loop node keeps its body). The report is addressed by `report_id`
    /// (not index) so a tab reorder can't misroute the edit. See
    /// [`crate::tui::report_nodes`].
    ReportNodeLine {
        report_id: u64,
        path: Vec<usize>,
    },
}

impl PromptKind {
    /// The ghost suffix to autocomplete in a file-save prompt (`.hurl` for a
    /// collection, `.vars` for an environment), or `""` for other prompts.
    pub(crate) fn save_ghost(&self) -> &'static str {
        match self {
            PromptKind::FilePath(FileAction::SaveCollection) => ".hurl",
            PromptKind::FilePath(FileAction::SaveEnv) => ".vars",
            PromptKind::NewWorkspaceCollection(_) => ".hurl",
            PromptKind::NewWorkspaceReport(_) => ".trail",
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
    /// What this picker is for — plain browsing, choosing where a brand-new
    /// request lands, or choosing where an existing request is moved/copied.
    /// Changes the Enter action and the on-screen hint.
    pub(crate) mode: WsPickerMode,
}

/// Why a [`WorkspacePickerState`] is open, driving its Enter action and hint.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum WsPickerMode {
    /// Browse the workspace and load the chosen collection file (the `w` key
    /// and the auto-open-on-empty-tab flow).
    Browse,
    /// Choose the file to append the parked
    /// [`TuiApp::pending_workspace_request`] to (load-then-append).
    AddRequest,
    /// Choose the file to move the parked
    /// [`TuiApp::pending_workspace_transfer`] into (written to disk, then
    /// removed from its source collection).
    MoveRequest,
    /// Choose the file to copy the parked
    /// [`TuiApp::pending_workspace_transfer`] into (written to disk; the
    /// original stays put).
    CopyRequest,
}

/// An existing workspace request parked for a move/copy, awaiting a destination
/// collection chosen through the workspace picker. See
/// [`TuiApp::pending_workspace_transfer`].
#[derive(Clone)]
pub(crate) struct PendingTransfer {
    /// The request being moved/copied (a clone of the source entry).
    pub(crate) entry: HurlEntry,
    /// `collections` index of the Workspace tab the request came from.
    pub(crate) source_ci: usize,
    /// Index of the request within the source collection's loaded `entries`.
    pub(crate) source_idx: usize,
    /// `true` for a move (remove from source after writing the destination),
    /// `false` for a copy (leave the source untouched).
    pub(crate) is_move: bool,
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
            mode: WsPickerMode::Browse,
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
    /// The "Load" submenu of the File menu: just the *kinds* (Request /
    /// Collection / Environment / Workspace). Picking a kind that can come
    /// from more than one place opens `FileLoadSource` to choose local vs
    /// git; Request (local-only) goes straight to its path prompt. Esc/q
    /// returns to `FileMenu(0)`.
    FileLoadMenu(usize),
    /// The "Save" submenu of the File menu: just the *kinds* (Request /
    /// Collection / Environment / Workspace / Response). Picking a kind with
    /// more than one destination opens `FileSaveDest`; Request/Response go
    /// straight to their path prompt. Esc/q returns to `FileMenu(1)`.
    FileSaveMenu(usize),
    /// Second step of "Load": where should this `FileKind` come from —
    /// `(L)ocal file…` or `From (G)it…`. Esc/q returns to `FileLoadMenu` with
    /// the kind re-highlighted.
    FileLoadSource(FileKind, usize),
    /// Second step of "Save": where should this `FileKind` go — `(S)ave` /
    /// `Save (A)s…` / `To (G)it…` (the exact set depends on the kind). Esc/q
    /// returns to `FileSaveMenu` with the kind re-highlighted.
    FileSaveDest(FileKind, usize),
    /// The "Settings" menu (Language / Theme / Preferences / Clear all
    /// collections) — opened with `s`. Not to be confused with `Preferences`,
    /// the submenu one level down that holds the actual toggle-able preferences.
    Options(usize),
    LanguageMenu(usize),
    /// The Theme editor (Settings → Theme): pick a preset/custom theme or build
    /// your own. Carries its whole working state (see
    /// [`crate::tui::theme_editor::ThemeEditorState`]).
    ThemeEditor(crate::tui::theme_editor::ThemeEditorState),
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
    /// Dry-run preview for a report tab: the projected row count, a sample of
    /// the first few iterations' resolved bindings, and any producer /
    /// request-resolution problems — computed by expanding the flow with a
    /// no-op runner (no HTTP). Opened with `d` in the Reports view; scrolls with
    /// Up/Down, closes with Esc/`q` (see [`crate::tui::reports::DryRunReport`]).
    ReportDryRun(Box<crate::tui::reports::DryRunReport>),
    /// Drill-down popup for a results grid cell: shows the selected cell's full
    /// (untruncated, unflattened) content in a scrollable, selectable panel.
    /// Opened with Enter (or a second click on the same cell) in the Results
    /// grid; closed with Esc. The `title` is the column header, `content` is the
    /// raw cell value (may be multi-line), and `panel` tracks scroll + text
    /// selection across frames.
    ReportCellPopup {
        title: String,
        content: String,
        panel: Box<tui_panel_select::MultiSelectPanel>,
    },
    /// Column-picker overlay for a report tab: an interactive checklist of the
    /// columns the last run produced (plus the flow's raw loop/assign
    /// variables). Toggling/reordering writes back to the flow's `# columns:`
    /// header directive. Opened with `c` in the Reports view; needs a prior run
    /// (available columns come from its result). See
    /// [`crate::tui::reports::ColumnPicker`].
    ReportColumns(Box<crate::tui::reports::ColumnPicker>),
    /// Collection-binding picker for a report tab: a list of the currently-open
    /// collection tabs; choosing one re-points the report's `# collection:`
    /// header at it (relative path preferred). Opened with `b` in the Reports
    /// view. See [`crate::tui::reports::ReportBindPicker`].
    ReportBind(Box<crate::tui::reports::ReportBindPicker>),
    /// The structured node editor's insert/pick menu ([`Overlay::ReportNodeMenu`]):
    /// a two-step palette that first picks a node *kind* to add, then — for a
    /// `REQUEST` / `REPORT REQUEST` — picks a request name from the bound
    /// collection. Opened with `a` (add) or `Enter`/`e` (edit a request node) in
    /// the node view. See [`crate::tui::report_nodes::NodeMenu`].
    ReportNodeMenu(Box<crate::tui::report_nodes::NodeMenu>),
    /// The structured node editor's reported-request detail form
    /// ([`Overlay::ReportNodeRequest`]): configures a `REPORT REQUEST` node's
    /// response format, `AS` alias and `SHOW(…)` field checklist. Opened with
    /// `f` on a `REPORT REQUEST` node. See
    /// [`crate::tui::report_nodes::RequestForm`].
    ReportNodeRequest(Box<crate::tui::report_nodes::RequestForm>),
    /// The structured node editor's `FOR … IN ENVS` configure form
    /// ([`Overlay::ReportNodeEnvs`]): picks the loop variable, the Iterate/
    /// Compare mode and the baseline/comparison environment names from the
    /// loaded environments (rather than typing them by hand). Opened with
    /// `Enter` on a `FOR … IN ENVS` node. See
    /// [`crate::tui::report_nodes::EnvsForm`].
    ReportNodeEnvs(Box<crate::tui::report_nodes::EnvsForm>),
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
    /// Shown before "Save Workspace to Git" when the currently-loaded file in
    /// the Workspace tab has unsaved in-memory edits: a git push commits the
    /// files as they sit on disk, so those edits would be left out unless
    /// saved first (switching files within a Workspace discards in-memory
    /// edits, so only the current file can ever hold them). `ci` is the
    /// Workspace tab. `sel`: 0 = save the edits to disk then push, 1 = push
    /// the on-disk version as-is (leaving the edits only in memory), 2 =
    /// cancel.
    WorkspaceGitSaveUnsaved {
        ci: usize,
        sel: usize,
    },
    /// Shown when the user opens a *different* collection in a Workspace tab
    /// while the currently-loaded one has unsaved in-memory edits: loading a
    /// file replaces the tab's entries wholesale, so those edits would be
    /// silently discarded. `ci` is the Workspace tab, `target` the collection
    /// file to switch to. `sel`: 0 = save the edits first then switch, 1 =
    /// discard the edits and switch, 2 = cancel (stay).
    WorkspaceSwitchUnsaved {
        ci: usize,
        target: PathBuf,
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
    /// Discard a request's in-memory edits, reloading it from the collection's
    /// on-disk file. Holds `(collection index, entry index)`. Raised by `Ctrl+R`
    /// in the Requests list only when that entry has unsaved changes.
    RevertRequest(usize, usize),
    /// Discard a Global Environment's unsaved edits, restoring the last-saved
    /// values. Holds the env id. Raised by `Ctrl+R` in the entries popup only
    /// when the env has unsaved changes.
    RevertEnv(u64),
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
/// can hold a text selection — Main before Response — so a cross-panel copy
/// concatenates them in reading order (see
/// `input::concatenated_selection_text`, which iterates the two panels in
/// this order directly).
/// The 2 top-level File menu items: "(L)oad" and "(S)ave".
pub(crate) fn file_menu_items(s: &Strings) -> [&'static str; 2] {
    [s.file_menu_item_load, s.file_menu_item_save]
}

/// The kinds of thing the File menu can load or save. Chosen in the first
/// step of Load/Save; the local-vs-git (or Save/Save As/Git) choice is a
/// second step (see [`Overlay::FileLoadSource`] / [`Overlay::FileSaveDest`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FileKind {
    Collection,
    Environment,
    Workspace,
    /// A PaperTrail `.trail` document (see [`crate::report::Report`]). Loaded
    /// into / saved from a report tab, locally or from a git remote (mirroring
    /// the collection flow).
    Report,
}

impl FileKind {
    /// The plain (mnemonic-free) name shown in a second-step popup title.
    pub(crate) fn name(self, s: &Strings) -> &'static str {
        match self {
            FileKind::Collection => s.file_kind_collection,
            FileKind::Environment => s.file_kind_environment,
            FileKind::Workspace => s.file_kind_workspace,
            FileKind::Report => s.file_kind_report,
        }
    }
}

/// The row this kind occupies in the "Load" kind list (see
/// [`file_load_items`]), used to re-highlight it when Esc steps back.
pub(crate) fn file_load_kind_index(kind: FileKind) -> usize {
    match kind {
        FileKind::Collection => 1,
        FileKind::Environment => 2,
        FileKind::Workspace => 3,
        FileKind::Report => 4,
    }
}

/// One selectable row in the File → Save submenu. Which rows appear depends
/// on what's currently open/focused (see [`TuiApp::file_save_items`]) so the
/// menu never offers a save that can't apply — e.g. "Save Request" while a
/// report tab is active (there's no request to write), or "Save Report" while
/// a collection tab is active.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SaveItem {
    Request,
    /// Collection / Environment / Workspace / Report — these route through the
    /// second-step destination menu ([`Overlay::FileSaveDest`]).
    Kind(FileKind),
    Response,
}

impl SaveItem {
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            SaveItem::Request => s.file_save_item_request,
            SaveItem::Kind(FileKind::Collection) => s.file_save_item_collection,
            SaveItem::Kind(FileKind::Environment) => s.file_save_item_environment,
            SaveItem::Kind(FileKind::Workspace) => s.file_save_item_workspace,
            SaveItem::Kind(FileKind::Report) => s.file_save_item_report,
            SaveItem::Response => s.file_save_item_response,
        }
    }
}

/// The 5 kinds of the File menu's "Load" submenu.
pub(crate) fn file_load_items(s: &Strings) -> [&'static str; 5] {
    [
        s.file_load_item_request,
        s.file_load_item_collection,
        s.file_load_item_environment,
        s.file_load_item_workspace,
        s.file_load_item_report,
    ]
}

/// The two "Load" source choices: a local file or a git remote.
pub(crate) fn file_load_source_items(s: &Strings) -> [&'static str; 2] {
    [s.file_source_local, s.file_source_git]
}

/// The "Save" destination choices for `kind`. Collections can be saved to
/// their file, to a new file, or to git; environments to their file or a new
/// file (no git save); a workspace is a folder, so only "save as a copy" or
/// git apply.
pub(crate) fn file_save_dest_items(kind: FileKind, s: &Strings) -> Vec<&'static str> {
    match kind {
        FileKind::Collection => vec![s.file_dest_save, s.file_dest_save_as, s.file_dest_git],
        FileKind::Environment => vec![s.file_dest_save, s.file_dest_save_as],
        FileKind::Workspace => vec![s.file_dest_save_as, s.file_dest_git],
        // Reports can be saved to their file, a new file, or a git remote.
        FileKind::Report => vec![s.file_dest_save, s.file_dest_save_as, s.file_dest_git],
    }
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
/// browser confirms a destination; the final destination is
/// `dest_dir.join(name)`, `name` coming from the browser's inline filename
/// editor (defaulting to `default_name`).
pub(crate) struct PendingWorkspaceSave {
    pub(crate) source_root: PathBuf,
    pub(crate) default_name: String,
    pub(crate) target: WorkspaceSaveTarget,
    pub(crate) dest_dir: Option<PathBuf>,
}

/// A tab that was closed and can be reopened with `u` / Ctrl+Shift+T. Unifying
/// collection and report tabs in one recency-ordered stack keeps the undo order
/// correct across both kinds (rather than two independent stacks that could
/// restore out of order). Each variant remembers the *within-kind* index it was
/// closed from so it reappears close to where it was.
pub(crate) enum ClosedTab {
    // Both payloads are large (a `Collection`, and a `ReportTab` carrying two
    // `MultiSelectPanel`s), so box both to keep the enum pointer-sized and the
    // variants balanced.
    Collection(usize, Box<Collection>),
    Report(usize, Box<crate::tui::reports::ReportTab>),
}

pub struct TuiApp {
    pub(crate) language: Language,
    pub(crate) vars: AppVars,
    pub(crate) collections: Vec<Collection>,
    pub(crate) active_tab: usize,
    /// Report tabs (PaperTrail `.trail` documents). *Standalone* reports show in
    /// the same tab bar after the collection tabs — the unified tab index
    /// (`active_tab`) counts collections first, then the standalone reports (see
    /// [`Self::standalone_report_indices`]). A report opened from a Workspace
    /// tree is instead *embedded* in its Workspace collection tab: it still lives
    /// here (so it reuses every report handler) but carries `workspace`, which
    /// keeps it out of the strip and links it to its collection tab by root — its
    /// `active_tab` is that collection index. See [`Self::active_is_report`] and
    /// [`Self::embedded_report_index`].
    pub(crate) reports: Vec<crate::tui::reports::ReportTab>,
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
    /// Undo stack for deleted Global Environments: each entry is the list index
    /// the environment was removed from plus the environment itself, so `u`
    /// (in the Global Environments panel) can reopen the most recent one. The
    /// exact parallel of a collection's `deleted_entries`.
    pub(crate) deleted_envs: Vec<(usize, Environment)>,

    pub(crate) focus: Pane,
    /// Selected row in the Global Environments list (panel showing env
    /// NAMES only — see `Pane::GlobalEnv`). Renamed from the old `env_idx`,
    /// which used to index the selected *variable* row inside the old
    /// inline Environment panel; that per-variable selection now lives in
    /// `Overlay::EnvPopup`'s own `EnvPopupState::idx`.
    pub(crate) global_env_idx: usize,
    /// Max scroll offset for the Response body (wrapped content rows −
    /// viewport height); cached each frame by `draw_response` from
    /// `resp_panel.clamp_scroll(..)` so a scrollbar drag between frames (and
    /// the footer) can read it without a live viewport height. The scroll
    /// offset itself now lives in `resp_panel`.
    pub(crate) resp_max_scroll: u16,
    /// Same, for the Request JSON/Hurl body; cached by `draw_collection_main`.
    /// The offset itself lives in `main_panel`.
    pub(crate) main_max_scroll: u16,
    /// Horizontal scroll offset (in characters) for the selected entry's name in
    /// the collections list, so long request URLs can be read end-to-end.
    pub(crate) list_hscroll: u16,
    /// Same, for the selected Global Environment's name in the Global
    /// Environments list (in case a name is longer than the panel is wide).
    pub(crate) global_env_hscroll: u16,
    /// The exact screen Rect the Request JSON body was rendered into last
    /// frame, used to hit-test mouse clicks/drags against this panel.
    pub(crate) main_text_area: Rect,
    /// The Request JSON/Hurl body panel: owns its scroll offset, wrap cache
    /// and text selection (active + Alt+Click+Drag regions). Its wrap is
    /// rebuilt fresh each frame by `draw_collection_main` (the content is
    /// always small) — the single source of truth for rendering, mapping
    /// mouse coordinates to text, and extracting a selection, so all three
    /// always agree on exactly the same content.
    pub(crate) main_panel: MultiSelectPanel,
    /// Character positions (within `main_panel`'s logical text) of every
    /// shadow-warning icon (see `draw::SHADOW_ICON`) rendered into the
    /// Request JSON/Hurl body this frame — recomputed each frame the panel's
    /// content is rebuilt. A purely visual annotation, so it's excluded from
    /// copied/selected text (see `whole_panel_text`,
    /// `concatenated_selection_text`) rather than corrupting a pasted
    /// request with a stray "!" the recipient would have to notice and
    /// remove by hand.
    pub(crate) main_shadow_icon_positions: std::collections::HashSet<TextPos>,
    /// The exact screen Rect the Response body was rendered into last frame,
    /// used to hit-test mouse clicks/drags against this panel.
    pub(crate) resp_text_area: Rect,
    /// The Response body panel: like `main_panel`, but its wrap cache is
    /// *not* rebuilt unconditionally every frame — only when the body or
    /// panel width actually changes (`set_content` → `rebuild_if_needed`),
    /// which is what keeps dragging a selection or scrolling responsive even
    /// for an "obscenely large" body.
    pub(crate) resp_panel: MultiSelectPanel,
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
    /// Screen text areas for the report view's three panels (Source,
    /// Validation, Results), recorded during draw for mouse hit-testing
    /// (selection begin/drag + scrollbar click/drag), analogous to
    /// `main_text_area`/`resp_text_area` but for the full-screen report view.
    /// Indexed by `ReportPane as usize`; `Rect::default()` (unhittable) for a
    /// panel not drawn this frame (e.g. the results grid while the source view
    /// is showing).
    pub(crate) report_pane_areas: [Rect; 3],
    /// The one-column scrollbar Rect for each report panel (same indexing as
    /// `report_pane_areas`), for scrollbar click-to-jump / drag-to-scroll.
    pub(crate) report_pane_bars: [Rect; 3],
    /// Set while dragging a report panel's scrollbar thumb (which panel), so a
    /// `Drag` keeps adjusting its scroll even if the cursor leaves the
    /// one-column track. Cleared on `Up`.
    pub(crate) report_scrollbar_drag: Option<crate::tui::reports::ReportPane>,
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
    /// Vertical scroll offset for the report dry-run preview overlay
    /// ([`Overlay::ReportDryRun`]) — reset to 0 when the preview is opened.
    /// Kept on the app (like [`Self::help_scroll`]) so the immutable overlay
    /// draw pass can clamp overshoot against the real content height.
    pub(crate) dry_run_scroll: u16,
    pub(crate) quit: bool,

    /// Receivers for in-flight background secret resolution (one per env load).
    pub(crate) pending_env: Vec<Receiver<EnvUpdate>>,

    /// Receivers for in-flight response captures (one per run of a capturing entry).
    pub(crate) pending_captures: Vec<Receiver<CaptureUpdate>>,

    /// Receivers for in-flight "Run All" (Alt+F5) passes over a whole collection.
    pub(crate) pending_batch_runs: Vec<Receiver<request::BatchRunUpdate>>,

    /// Receivers for in-flight background report runs (one per running report),
    /// each tagged with its report id, drained by
    /// [`Self::poll_report_run_updates`]. A report runs on its own thread so the
    /// whole app stays responsive during a long run.
    pub(crate) pending_report_runs: Vec<(u64, Receiver<crate::tui::reports::ReportRunUpdate>)>,
    /// Cancel flags for the reports currently running, keyed by report id (a
    /// process-unique identity that survives tab reordering). The background
    /// worker's runner checks this between requests and short-circuits when it
    /// flips, so a run can be cancelled mid-flight; the delivered result is then
    /// discarded. Presence of a key also gates a second run of the same report.
    pub(crate) running_reports:
        std::collections::HashMap<u64, std::sync::Arc<std::sync::atomic::AtomicBool>>,

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

    /// Deepest folder the file browser was in before the user started walking
    /// *up* the tree with Left, i.e. the trail to retrace on the way back down.
    /// Each Left keeps it; each Right that follows the trail re-selects the next
    /// folder down it, so pressing Left N times then Right N times returns to
    /// exactly where you started. Descending into any *other* folder (a genuine
    /// new navigation) clears it. Only meaningful while a browser overlay is up.
    pub(crate) browser_forward_path: Option<PathBuf>,

    /// Settings (persisted): confirm before quitting / closing all collections.
    pub(crate) confirm_on_exit: bool,
    pub(crate) confirm_on_clear: bool,
    /// Preferences (persisted): confirm before deleting a Global Environment
    /// (`x` in the Global Environments panel). On by default; turn it off to
    /// always delete immediately (the deletion stays undoable with `u`).
    pub(crate) confirm_on_delete_env: bool,
    /// Preferences (persisted): when set, a "Save / Discard / Cancel" prompt
    /// for unsaved in-memory edits (switching collections in a Workspace, or
    /// pushing one to git) is skipped and the "Save" action taken
    /// automatically. Off by default, so the prompt is shown.
    pub(crate) always_save_when_prompted: bool,
    /// Preferences (persisted): which of JSON / Hurl text the Main (Request)
    /// panel shows by default, for every request. Changed from the
    /// Preferences submenu (Settings → Preferences → Default Request View).
    pub(crate) default_request_view: request::RequestView,
    /// Preferences (persisted): run "Run All" (Alt+F5) in batch mode — the
    /// whole collection in one Hurl execution, so Hurl's cookie jar and
    /// `[Captures]` chain across every request. Off by default, so Run All
    /// streams results as they finish (matching the CLI default), at the cost
    /// of not carrying automatic cookies between requests.
    pub(crate) run_all_batch_mode: bool,

    /// User-created themes (persisted). Shown in the Theme editor alongside the
    /// built-in presets; deletable (unlike presets) with `Ctrl+D`.
    pub(crate) custom_themes: Vec<crate::tui::theme::ThemeSpec>,
    /// The explicitly-chosen theme name, or `None` to follow the language's
    /// preset. Set the moment the user picks any theme in the Theme editor;
    /// while `None`, changing language also changes the effective theme.
    pub(crate) active_theme: Option<String>,

    /// `true` when the terminal supports the keyboard-enhancement protocol, so
    /// Ctrl+Enter is reported distinctly from a plain Enter. Advanced shortcuts
    /// (Ctrl+Enter) are only advertised in the UI when this is set.
    pub(crate) enhanced_keys: bool,

    /// Target path for a pending "Save As" overwrite confirmation.
    pub(crate) pending_save_path: Option<PathBuf>,

    /// A brand-new request awaiting a destination inside a Workspace tab. Set
    /// when a "New Request" form targets a Workspace (whose entries belong to
    /// whichever file is loaded, so there's no single obvious destination);
    /// the workspace destination picker then either appends it to the file
    /// the user opens or seeds a new collection created with `n`. Taken (set
    /// back to `None`) as soon as it lands somewhere or the picker is
    /// cancelled, so an aborted flow never leaks state.
    pub(crate) pending_workspace_request: Option<HurlEntry>,

    /// An existing workspace request awaiting a move/copy destination. Set when
    /// the user presses `m` (move) or `c` (copy) on a request row of a
    /// Workspace tab; the workspace destination picker then writes the entry
    /// into whichever collection file the user opens (and, for a move, removes
    /// it from its source). Taken (set back to `None`) as soon as the transfer
    /// commits or the picker is cancelled, so an aborted flow never leaks
    /// state.
    pub(crate) pending_workspace_transfer: Option<PendingTransfer>,

    /// Git URLs the user has loaded a collection/environment from, most recent
    /// first. Offered as a pickable list in the "Load from Git" wizard and
    /// persisted across restarts.
    pub(crate) recent_git_urls: Vec<String>,

    /// Stack of recently closed tabs (with the index they were closed from),
    /// most-recently-closed last, so Ctrl+Shift+T can reopen them in order.
    /// Holds both collection and report tabs (see [`ClosedTab`]) so undo order
    /// stays correct across the two kinds. Runtime-only (not persisted).
    pub(crate) closed_tabs: Vec<ClosedTab>,

    /// The in-progress New/Edit Request wizard, stashed here while the file
    /// browser is open for `FileAction::PickFormFile` (the overlay is a
    /// single slot, so opening the Browser would otherwise discard it).
    /// Restored (with the picked path applied, on success) once the browser
    /// closes. Runtime-only (not persisted).
    pub(crate) parked_wizard: Option<Box<NewReq>>,
    /// The target `FOR … IN FILES/FOLDERS` node whose source folder is being
    /// chosen while a [`FileAction::PickReportNodeFolder`] browser is open:
    /// `(report id, node path)`. The chosen directory is written into that
    /// loop's producer `dir` on `Space`. Runtime-only (not persisted).
    pub(crate) pending_node_folder: Option<(u64, Vec<usize>)>,
    /// The inline filename editor shown at the bottom of a "save to folder"
    /// browser (the two `*ChooseFolder` [`FileAction`]s): the file name for a
    /// collection, or the workspace's own subfolder name. Seeded with a
    /// sensible default when the browser opens; meaningless for other browser
    /// actions. Runtime-only (not persisted).
    pub(crate) browser_name: Editor,
    /// Whether keyboard focus in a "save to folder" browser is on
    /// `browser_name` (reached with Tab) rather than the folder list. Enter on
    /// the focused filename saves into the current folder. Runtime-only.
    pub(crate) browser_name_focused: bool,
    /// Whether the local load browser (Open Collection / Load Environment /
    /// Open Report) is hiding files that don't match the action's extension set
    /// (`.hurl`/`.json`, `.vars`/`.env*`, `.trail`). On by default; `Tab`
    /// toggles it so an oddly-named file can still be picked. Runtime-only.
    pub(crate) browser_filter_on: bool,
    /// Which pane focus returns to when the New/Edit Request wizard closes
    /// (whether saved or cancelled). Opening the wizard temporarily moves
    /// focus onto the Main panel so the request preview shows behind it, but
    /// on close we restore the pane the user launched it from — normally the
    /// Requests list — rather than stranding them on the raw request view.
    /// Runtime-only (not persisted).
    pub(crate) wizard_return_focus: Pane,
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
            reports: Vec::new(),
            response: Arc::new(Mutex::new(ApiResponse::default())),
            global_envs: Vec::new(),
            active_env_id: None,
            deleted_envs: Vec::new(),
            focus: Pane::List,
            global_env_idx: 0,
            resp_max_scroll: 0,
            main_max_scroll: 0,
            list_hscroll: 0,
            global_env_hscroll: 0,
            main_text_area: Rect::default(),
            main_panel: MultiSelectPanel::new(),
            main_shadow_icon_positions: std::collections::HashSet::new(),
            resp_text_area: Rect::default(),
            resp_panel: MultiSelectPanel::new(),
            main_scrollbar_area: Rect::default(),
            resp_scrollbar_area: Rect::default(),
            scrollbar_drag: None,
            report_pane_areas: [Rect::default(); 3],
            report_pane_bars: [Rect::default(); 3],
            report_scrollbar_drag: None,
            prompt_editor_area: Rect::default(),
            list_scroll_w: std::cell::Cell::new(0),
            global_env_scroll_w: std::cell::Cell::new(0),
            response_pct: 42,
            list_width: 38,
            status: None,
            overlay: None,
            help_scroll: 0,
            dry_run_scroll: 0,
            quit: false,
            pending_env: Vec::new(),
            pending_captures: Vec::new(),
            pending_batch_runs: Vec::new(),
            pending_report_runs: Vec::new(),
            running_reports: std::collections::HashMap::new(),
            last_browse_dir: None,
            last_env_dir: None,
            browser_origin_dir: None,
            browser_forward_path: None,
            confirm_on_exit: true,
            confirm_on_clear: true,
            confirm_on_delete_env: true,
            always_save_when_prompted: false,
            default_request_view: request::RequestView::default(),
            run_all_batch_mode: false,
            custom_themes: Vec::new(),
            active_theme: None,
            enhanced_keys: false,
            pending_save_path: None,
            pending_workspace_request: None,
            pending_workspace_transfer: None,
            recent_git_urls: Vec::new(),
            closed_tabs: Vec::new(),
            parked_wizard: None,
            pending_node_folder: None,
            browser_name: Editor::new("", false),
            browser_name_focused: false,
            browser_filter_on: true,
            wizard_return_focus: Pane::List,
            pending_collision_env: None,
            pending_workspace_reloads: std::collections::VecDeque::new(),
            workspace_redownload_rx: None,
            pending_workspace_save: None,
        }
    }
}

impl TuiApp {
    /// Whether there's any text selection at all — in either the Request
    /// (`main_panel`) or Response (`resp_panel`) body, active or finalized.
    /// Used to gate the `y`/Esc shortcuts.
    pub(crate) fn has_any_selection(&self) -> bool {
        self.main_panel.has_selection()
            || self.resp_panel.has_selection()
            || self
                .active_report()
                .map(|rt| {
                    rt.source_panel.has_selection()
                        || rt.validation_panel.has_selection()
                        || rt.results_panel.has_selection()
                })
                .unwrap_or(false)
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
        self.main_panel.clear();
        self.resp_panel.clear();
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
        self.resp_panel.set_scroll(0);
        // A fresh response is coming; any selection painted over the old
        // one would be stale.
        self.clear_selections();
        self.begin_request();
        if let Some(rx) = request::run_collection(
            &self.collections[col_idx],
            env.as_ref(),
            self.response.clone(),
        ) {
            self.pending_captures.push(rx);
        }
    }

    /// "Run All" (Alt+F5): run every request in the collection, in order.
    /// Streams results as each request finishes by default (the CLI default),
    /// or runs the whole collection in one Hurl execution when the
    /// `run_all_batch_mode` preference is set — in which case captures and the
    /// cookie jar chain across the whole run automatically.
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
        self.resp_panel.set_scroll(0);
        // A fresh response is coming; any selection painted over the old
        // one would be stale.
        self.clear_selections();
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
            self.run_all_batch_mode,
        ) {
            // Streaming Run All doesn't carry Hurl's automatic cookie jar
            // between requests — warn about it in the status bar (batch mode
            // is unaffected). Overwritten by the pass/fail summary once the
            // run finishes.
            if !self.run_all_batch_mode {
                self.status = Some(Status::RunAllStreamingCookies);
            }
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
                // including query_params/form_fields/cookies/basic_auth). On
                // failure, reopen the overlay with the user's text intact so
                // they can fix it.
                let entries = crate::hurl::parse_hurl(&text);
                if entries.len() != 1 {
                    let s = Strings::for_language(&self.language);
                    // Prefer the concrete parse reason (line + what's wrong)
                    // over the generic "expected exactly one request" — the
                    // latter only really fits the case where the text parses
                    // but holds zero or several requests.
                    let msg = crate::hurl::parse_hurl_error(&text)
                        .map(|why| format!("{} {why}", s.invalid_hurl_prefix))
                        .unwrap_or_else(|| s.invalid_hurl.to_string());
                    self.status = Some(Status::Error(msg));
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
                            || entry.queries != parsed.queries
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
                            || entry.queries != parsed.queries
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
            PromptKind::NewWorkspaceCollection(ci) => self.create_workspace_collection(ci, text),
            PromptKind::NewWorkspaceReport(ci) => self.create_workspace_report(ci, text),
            PromptKind::ReportNodeLine { report_id, path } => {
                self.commit_report_node_line(report_id, &path, text)
            }
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

    /// Discard the selected request's in-memory edits by reloading the single
    /// entry at the same position from the collection's on-disk file (#19).
    /// Returns the reverted request's HTTP method on success, or `None` when
    /// there's nothing to revert to — the collection has no file (scratch), the
    /// file can't be read/parsed, or it holds no entry at that position (e.g. a
    /// never-saved request). The other entries and their edits are untouched.
    pub(crate) fn revert_request_to_saved(&mut self, ci: usize, ei: usize) -> Option<String> {
        let path = self.collections.get(ci)?.path.clone()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut disk = crate::postman::parse_collection(&content);
        if ei >= disk.len() {
            return None;
        }
        let entry = disk.swap_remove(ei);
        let method = entry.method.clone();
        let col = self.collections.get_mut(ci)?;
        col.entries[ei] = entry; // a freshly parsed entry is clean (not modified/added)
        col.invalidate_request_json();
        col.sync_folder_to_selected();
        Some(method)
    }

    /// Discard a Global Environment's unsaved edits, restoring the last-saved
    /// disk values (#19): every modified var goes back to its `original_value`
    /// and every user-added var (not in the file) is dropped. Returns the env's
    /// name on success, or `None` when there's nothing to revert (unknown id,
    /// no file, or no changes).
    pub(crate) fn revert_env_to_saved(&mut self, env_id: u64) -> Option<String> {
        let env = self.global_envs.iter_mut().find(|e| e.id == env_id)?;
        if env.path.is_none() || !env.vars.iter().any(|v| v.user_added || v.modified) {
            return None;
        }
        env.vars.retain(|v| !v.user_added);
        for v in &mut env.vars {
            v.value = v.original_value.clone();
            v.modified = false;
        }
        let name = env.name.clone();
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.save_state();
        Some(name)
    }

    pub(crate) fn do_file_action(&mut self, action: FileAction, path: &str) {
        if path.is_empty() {
            return;
        }
        match action {
            FileAction::SaveRequest => {
                // `active_tab` counts collections then reports, so when a report
                // tab is active it indexes past `collections` — guard with
                // `.get()` (not `[]`) so "Save Request" with no active request
                // is a no-op status, not an out-of-bounds panic.
                let Some(col) = self.collections.get(self.active_tab) else {
                    self.status = Some(Status::NoResponse);
                    return;
                };
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
                if let Some(parent) = PathBuf::from(path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = std::fs::create_dir_all(parent);
                }
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
                        // Remember the folder so the next "Save Env As" (and the
                        // env load picker) reopens here.
                        if let Some(parent) = Path::new(path).parent() {
                            self.last_env_dir = Some(parent.to_path_buf());
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

                        if row.kind == FormFieldKind::Base64File && !inferred.is_empty() {
                            row.ctype = Editor::new("", false);
                            let prefix_default = format!("data:{inferred};base64,");
                            row.base64_prefix = Editor::new(&prefix_default, false);
                        } else {
                            row.base64_prefix = Editor::new("", false);
                            row.ctype = Editor::new(inferred, false);
                        }
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
            // A folder-only picker like the two above: the destination is
            // confirmed with `Space` (`collection_save_pick_folder` in
            // `input.rs`), which then routes the real write through
            // `FileAction::SaveCollection`, so this never reaches here.
            FileAction::SaveCollectionChooseFolder => {}
            // Like the folder pickers above: the report-CSV export confirms its
            // destination in `input.rs` (`browser_commit_save`), which writes
            // through `write_active_report_csv`, so this never reaches here.
            FileAction::SaveReportCsvChooseFolder => {}
            // Like the report-CSV export: the baseline-snapshot save confirms
            // its destination in `input.rs` (`browser_commit_save`), which
            // writes through `write_active_report_baseline`, so this never
            // reaches here.
            FileAction::SaveReportBaselineChooseFolder => {}
            FileAction::OpenReport => match crate::report::Report::load_local(path) {
                Ok(report) => self.open_loaded_report(report),
                Err(e) => self.status = Some(Status::Error(e)),
            },
            FileAction::SaveReport => {
                let Some(idx) = self.active_report_index() else {
                    self.status = Some(Status::NotReport);
                    return;
                };
                match self.reports[idx].report.save_local(path) {
                    Ok(()) => {
                        self.save_state();
                        self.status = Some(Status::Saved);
                    }
                    Err(e) => self.status = Some(Status::Error(e)),
                }
            }
            // Like the folder pickers above: the report "Save As" destination is
            // confirmed in `input.rs` (`browser_commit_save`), which routes the
            // real write through `FileAction::SaveReport`, so this never reaches
            // here.
            FileAction::SaveReportChooseFolder => {}
            // Like the folder pickers above: the loop's source folder is
            // confirmed with `Space` in `input.rs`
            // (`commit_report_node_folder`), so a file-Enter never reaches here.
            FileAction::PickReportNodeFolder => {}
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

    /// Save the workspace under its inline filename editor name: copy
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
                    // Cache the outgoing file's request names first, so if it was
                    // left expanded it keeps listing its requests from the cache
                    // once it is no longer the loaded file.
                    col.snapshot_loaded_titles();
                    col.entries = entries;
                    col.selected_entry = 0;
                    col.path = Some(path);
                    col.workspace_root = workspace_root;
                    col.workspace_filter_hurl_json = filter;
                    col.invalidate_request_json();
                    col.sync_folder_to_selected();
                    // Expand all ancestor folders of the newly-loaded file so
                    // it is visible in the tree (and un-collapse its accordion).
                    col.expand_ancestors_for_path();
                    col.sync_ws_cursor();
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

    /// Open the Workspace file-tree popup for tab `ci` in "choose a
    /// destination for the pending new request" mode (see
    /// [`TuiApp::pending_workspace_request`]). Identical to
    /// `open_workspace_picker_for_active_tab` except it also focuses the tab
    /// and flags the picker so Enter *appends* the parked request to whatever
    /// file is opened (rather than merely loading it), and the hint reflects
    /// that. A no-op if `ci` isn't a Workspace-bound tab (the request is left
    /// parked; callers guard on this).
    pub(crate) fn open_workspace_dest_picker(&mut self, ci: usize) {
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        let filter = col.workspace_filter_hurl_json;
        self.active_tab = ci;
        let mut picker = WorkspacePickerState::new(ci, root, filter);
        picker.mode = if self.pending_workspace_request.is_some() {
            WsPickerMode::AddRequest
        } else {
            WsPickerMode::Browse
        };
        self.overlay = Some(Overlay::WorkspacePicker(picker));
    }

    /// Open the Workspace file-tree popup for tab `ci` to choose a destination
    /// collection for a request being moved (`is_move == true`) or copied
    /// (`is_move == false`). The request itself must already be parked in
    /// [`TuiApp::pending_workspace_transfer`]. On Enter, the picker calls
    /// [`TuiApp::commit_workspace_transfer`] with the chosen file — writing it
    /// straight to disk — rather than loading it. A no-op if `ci` isn't a
    /// Workspace-bound tab.
    pub(crate) fn open_workspace_transfer_picker(&mut self, ci: usize, is_move: bool) {
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        let filter = col.workspace_filter_hurl_json;
        self.active_tab = ci;
        let mut picker = WorkspacePickerState::new(ci, root, filter);
        picker.mode = if is_move {
            WsPickerMode::MoveRequest
        } else {
            WsPickerMode::CopyRequest
        };
        self.overlay = Some(Overlay::WorkspacePicker(picker));
    }

    /// Commit the parked [`TuiApp::pending_workspace_transfer`] into the
    /// collection file at `dest_path`, writing straight to disk (no separate
    /// Save). For a copy the source is left untouched; for a move the request
    /// is also removed from its source collection (in memory and on disk).
    /// Moving/copying a request onto its own source file is handled specially:
    /// a move is a no-op, a copy duplicates the entry within the file. Returns
    /// after setting an appropriate status (success or error).
    pub(crate) fn commit_workspace_transfer(&mut self, dest_path: PathBuf) {
        let Some(transfer) = self.pending_workspace_transfer.take() else {
            return;
        };
        let PendingTransfer {
            entry,
            source_ci,
            source_idx,
            is_move,
        } = transfer;
        let method = entry.method.clone();
        let dest_name = dest_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dest_path.display().to_string());

        // Is the destination the same file currently loaded in the source tab?
        let same_file = self
            .collections
            .get(source_ci)
            .and_then(|c| c.path.as_ref())
            .map(|p| p == &dest_path)
            .unwrap_or(false);

        if same_file {
            if is_move {
                // Moving within the same file is a no-op.
                self.status = Some(Status::RequestMoved(method, dest_name));
                return;
            }
            // Copy within the loaded file: duplicate in memory, then persist.
            let Some(col) = self.collections.get_mut(source_ci) else {
                return;
            };
            let mut clone = entry;
            clone.user_added = true;
            clone.modified = true;
            col.entries.push(clone);
            let text = crate::hurl::collection_to_hurl(&col.entries);
            if let Err(e) = std::fs::write(&dest_path, text) {
                self.status = Some(Status::Error(e.to_string()));
                return;
            }
            self.mark_collection_saved(source_ci);
            if let Some(col) = self.collections.get_mut(source_ci) {
                col.invalidate_request_json();
                col.sync_ws_cursor();
            }
            self.save_state();
            self.status = Some(Status::RequestCopied(method, dest_name));
            return;
        }

        // Destination is a different file on disk. Read it, append, write back.
        let mut dest_entries: Vec<HurlEntry> = match std::fs::read_to_string(&dest_path) {
            Ok(text) => crate::postman::parse_collection(&text),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                self.status = Some(Status::Error(e.to_string()));
                return;
            }
        };
        let mut appended = entry.clone();
        appended.user_added = true;
        appended.modified = true;
        dest_entries.push(appended);
        if let Some(parent) = dest_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.status = Some(Status::Error(e.to_string()));
            return;
        }
        let dest_text = crate::hurl::collection_to_hurl(&dest_entries);
        if let Err(e) = std::fs::write(&dest_path, dest_text) {
            self.status = Some(Status::Error(e.to_string()));
            return;
        }

        if is_move {
            // Remove from the source collection (in memory + on disk).
            if let Some(col) = self.collections.get_mut(source_ci)
                && source_idx < col.entries.len()
            {
                col.entries.remove(source_idx);
                if col.selected_entry >= col.entries.len() {
                    col.selected_entry = col.entries.len().saturating_sub(1);
                }
                if let Some(path) = col.path.clone() {
                    let text = crate::hurl::collection_to_hurl(&col.entries);
                    if let Err(e) = std::fs::write(&path, text) {
                        self.status = Some(Status::Error(e.to_string()));
                        return;
                    }
                }
                self.mark_collection_saved(source_ci);
                if let Some(col) = self.collections.get_mut(source_ci) {
                    col.invalidate_request_json();
                    col.sync_folder_to_selected();
                    col.sync_ws_cursor();
                }
            }
            self.save_state();
            self.status = Some(Status::RequestMoved(method, dest_name));
        } else {
            self.save_state();
            self.status = Some(Status::RequestCopied(method, dest_name));
        }
    }

    /// Append the parked [`TuiApp::pending_workspace_request`], if any, to
    /// the collection currently loaded in Workspace tab `ci`, marking it as a
    /// user-added, unsaved edit and selecting it so it's immediately visible.
    /// Meant to be called right after `load_workspace_file` succeeded. The
    /// request is only taken once it's been placed; on a missing tab it's
    /// left parked for the caller to handle.
    pub(crate) fn append_pending_request_to_loaded(&mut self, ci: usize) {
        let Some(mut entry) = self.pending_workspace_request.take() else {
            return;
        };
        let Some(col) = self.collections.get_mut(ci) else {
            self.pending_workspace_request = Some(entry);
            return;
        };
        entry.user_added = true;
        entry.modified = true;
        col.entries.push(entry);
        col.selected_entry = col.entries.len() - 1;
        col.invalidate_request_json();
        col.sync_folder_to_selected();
        col.sync_ws_cursor();
        self.active_tab = ci;
        self.focus = Pane::Main;
        self.status = None;
        self.save_state();
    }

    /// Open the "name a new collection" prompt for Workspace tab `ci` (see
    /// [`PromptKind::NewWorkspaceCollection`]). The typed text is a path
    /// relative to the workspace root; `.hurl` is ghosted as the default
    /// extension.
    pub(crate) fn open_new_workspace_collection_prompt(&mut self, ci: usize) {
        if self
            .collections
            .get(ci)
            .and_then(|c| c.workspace_root.as_ref())
            .is_none()
        {
            return;
        }
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::NewWorkspaceCollection(ci),
            editor: Editor::blank(),
            title: s.workspace_new_collection_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Open the "name a new report" prompt for Workspace tab `ci` (see
    /// [`PromptKind::NewWorkspaceReport`]). The typed text is a path relative
    /// to the workspace root; `.trail` is ghosted as the default extension.
    /// Mirrors [`Self::open_new_workspace_collection_prompt`].
    pub(crate) fn open_new_workspace_report_prompt(&mut self, ci: usize) {
        if self
            .collections
            .get(ci)
            .and_then(|c| c.workspace_root.as_ref())
            .is_none()
        {
            return;
        }
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::NewWorkspaceReport(ci),
            editor: Editor::blank(),
            title: s.workspace_new_report_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Create a brand-new (in-memory, not-yet-written) collection file inside
    /// Workspace tab `ci`'s root at the relative path `rel`. Subfolders are
    /// allowed; a missing extension defaults to `.hurl`. The parked
    /// [`TuiApp::pending_workspace_request`], if any, becomes the collection's
    /// sole (user-added) entry so the just-created request is visible. The
    /// tab now shows this new file (unsaved — Ctrl+S writes it, creating any
    /// parent folders). Paths that are absolute or try to escape the root via
    /// `..` are rejected.
    pub(crate) fn create_workspace_collection(&mut self, ci: usize, rel: String) {
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        let filter = col.workspace_filter_hurl_json;
        let rel = rel.trim();
        if rel.is_empty() {
            return;
        }
        let mut rel_path = PathBuf::from(rel);
        if rel_path.extension().is_none() {
            rel_path.set_extension("hurl");
        }
        // Reject absolute paths or any `..`/root component that would let the
        // new file escape the workspace root.
        let safe = rel_path.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
        if !safe {
            self.status = Some(Status::Error(rel_path.display().to_string()));
            return;
        }
        let full_path = root.join(&rel_path);
        let mut entries = Vec::new();
        if let Some(mut entry) = self.pending_workspace_request.take() {
            entry.user_added = true;
            entry.modified = true;
            entries.push(entry);
        }
        let has_entry = !entries.is_empty();
        if let Some(col) = self.collections.get_mut(ci) {
            col.entries = entries;
            col.selected_entry = 0;
            col.path = Some(full_path);
            col.workspace_root = Some(root);
            col.workspace_filter_hurl_json = filter;
            col.invalidate_request_json();
            col.sync_folder_to_selected();
        }
        self.active_tab = ci;
        self.focus = if has_entry { Pane::Main } else { Pane::List };
        self.status = Some(Status::WorkspaceCollectionCreated(
            rel_path.display().to_string(),
        ));
        self.save_state();
    }

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

    /// The rows to show in the File → Save submenu, filtered to what actually
    /// applies to the current context so the menu never offers an inapplicable
    /// (or unsafe) save. A collection tab offers Request + Collection (plus
    /// Workspace when it's workspace-backed); a standalone report tab offers
    /// Report only; Environment and Response appear whenever there's an env or a
    /// response to write. A report *embedded in a Workspace tab* is a special
    /// case: its `active_tab` is the collection tab, so it offers Report
    /// alongside the tab's own Collection/Workspace saves — but NOT "Save
    /// Request" (the right pane is the report, not a request being edited). The
    /// order mirrors the original fixed list — rows that don't apply are simply
    /// omitted.
    pub(crate) fn file_save_items(&self) -> Vec<SaveItem> {
        let report_active = self.active_report_index().is_some();
        // The active collection tab underneath the current view, if any. For a
        // standalone report tab `active_tab` is past the collections so this is
        // `None`; for an embedded report it resolves to its Workspace tab, which
        // *can* target a collection/workspace.
        let collection = self.collections.get(self.active_tab);
        let mut items = Vec::new();
        // "Save Request" only when a request is actually on screen — never while
        // a report occupies the right pane (embedded), and never on a report
        // strip tab (no collection).
        if collection.is_some() && !report_active {
            items.push(SaveItem::Request);
            items.push(SaveItem::Kind(FileKind::Collection));
        } else if collection.is_some() {
            // Embedded report: still offer Collection (the tab's own file), just
            // not Request.
            items.push(SaveItem::Kind(FileKind::Collection));
        }
        if self.current_env_id().is_some() {
            items.push(SaveItem::Kind(FileKind::Environment));
        }
        if collection.is_some_and(|c| c.workspace_root.is_some()) {
            items.push(SaveItem::Kind(FileKind::Workspace));
        }
        if report_active {
            items.push(SaveItem::Kind(FileKind::Report));
        }
        if !self.response.lock().unwrap().body.is_empty() {
            items.push(SaveItem::Response);
        }
        items
    }

    /// The row `SaveItem::Kind(kind)` occupies in the current (filtered) Save
    /// list, used to re-highlight it when Esc steps back from the destination
    /// menu. The kind is always present (you reached the destination step by
    /// selecting it), so this resolves; it falls back to `0` defensively.
    pub(crate) fn file_save_kind_index(&self, kind: FileKind) -> usize {
        self.file_save_items()
            .iter()
            .position(|it| *it == SaveItem::Kind(kind))
            .unwrap_or(0)
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
    /// becomes unlinked, and it's deactivated if it was active. The removed
    /// environment is pushed onto `deleted_envs` so `u` can reopen it, and a
    /// status naming it (with the undo hint) is shown.
    pub(crate) fn delete_global_env(&mut self, idx: usize) {
        if idx >= self.global_envs.len() {
            return;
        }
        let removed = self.global_envs.remove(idx);
        let id = removed.id;
        let name = removed.name.clone();
        if self.active_env_id == Some(id) {
            self.active_env_id = None;
        }
        for col in &mut self.collections {
            if col.linked_env_id == Some(id) {
                col.linked_env_id = None;
            }
        }
        self.deleted_envs.push((idx, removed));
        self.global_env_idx = self
            .global_env_idx
            .min(self.global_envs.len().saturating_sub(1));
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.status = Some(crate::i18n::Status::EnvDeleted(name));
        self.save_state();
    }

    /// Reopen the most recently deleted Global Environment (`u`, Global
    /// Environments panel), restoring it as close as possible to the index it
    /// was removed from and selecting it. The parallel of
    /// [`Self::restore_deleted_request`] for environments.
    pub(crate) fn restore_deleted_env(&mut self) {
        let Some((idx, env)) = self.deleted_envs.pop() else {
            return;
        };
        let idx = idx.min(self.global_envs.len());
        let name = env.name.clone();
        self.global_envs.insert(idx, env);
        self.global_env_idx = idx;
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.status = Some(crate::i18n::Status::EnvReopened(name));
        self.save_state();
    }

    /// Every theme offered in the Theme editor, in display order: the built-in
    /// presets first, then the user's own custom themes.
    pub(crate) fn all_themes(&self) -> Vec<crate::tui::theme::ThemeSpec> {
        let mut themes = crate::tui::theme::builtin_presets();
        themes.extend(self.custom_themes.iter().cloned());
        themes
    }

    /// Look a theme up by name across presets and custom themes.
    pub(crate) fn find_theme(&self, name: &str) -> Option<crate::tui::theme::ThemeSpec> {
        self.all_themes().into_iter().find(|t| t.name == name)
    }

    /// The theme spec currently in effect: the manually-chosen theme if set
    /// (and still present), otherwise the current language's preset.
    pub(crate) fn active_theme_spec(&self) -> crate::tui::theme::ThemeSpec {
        if let Some(name) = &self.active_theme
            && let Some(spec) = self.find_theme(name)
        {
            return spec;
        }
        crate::tui::theme::preset_for_language(&self.language)
    }

    /// The runtime [`Theme`](crate::tui::theme::Theme) to draw with. While the
    /// Theme editor is open its live draft is returned instead, so every colour
    /// tweak previews across the whole UI immediately.
    pub(crate) fn theme(&self) -> crate::tui::theme::Theme {
        if let Some(Overlay::ThemeEditor(state)) = &self.overlay {
            return state.draft.to_theme();
        }
        self.active_theme_spec().to_theme()
    }
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

    /// Open the "Save Report to Git…" wizard for the active report tab, or show
    /// [`Status::NoGitOrigin`] if it wasn't loaded from git (mirrors
    /// [`Self::open_git_save_wizard`] — pushing to a brand-new remote is not
    /// offered here, only repinning a report that already has a git origin).
    pub(crate) fn open_git_save_report_wizard(&mut self) {
        let Some(idx) = self.active_report_index() else {
            self.status = Some(Status::NotReport);
            return;
        };
        if self.reports[idx].report.git_origin.is_none() {
            self.status = Some(Status::NoGitOrigin);
            return;
        }
        self.overlay = Some(Overlay::GitSave(Box::new(GitSaveWizard::new_report(
            idx,
            &self.reports[idx].report,
        ))));
    }

    /// Open the "Save Workspace to Git…" wizard for the active tab, or show
    /// [`Status::NoGitOrigin`] if it isn't a git-loaded Workspace (this action
    /// is only ever offered for a tab that was downloaded from git and still
    /// has its files on disk). If the currently-loaded file has unsaved
    /// in-memory edits, a warning is shown first (see
    /// [`Overlay::WorkspaceGitSaveUnsaved`]) so those edits aren't silently
    /// omitted from the pushed on-disk tree.
    pub(crate) fn open_git_workspace_save_wizard(&mut self) {
        let ci = self.active_tab;
        let col = &self.collections[ci];
        let is_git_workspace = col.workspace_git_origin.is_some() && col.workspace_root.is_some();
        if !is_git_workspace {
            self.status = Some(Status::NoGitOrigin);
            return;
        }
        // The push commits the tree as it sits on disk; warn if the loaded
        // file has edits that only live in memory (and thus a place on disk
        // to save them to) so the user can choose whether to include them.
        if col.path.is_some() && self.changed_request_count(ci) > 0 {
            // With "always save" on, auto-pick Save (push only if the write
            // succeeded); otherwise ask.
            if self.always_save_when_prompted {
                if self.save_workspace_current_file(ci) {
                    self.start_git_workspace_save_wizard(ci);
                }
                return;
            }
            self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved { ci, sel: 0 });
            return;
        }
        self.start_git_workspace_save_wizard(ci);
    }

    /// Open the workspace git-save wizard for tab `ci` (assumes it is a
    /// git-loaded Workspace). Factored out so the unsaved-changes warning's
    /// "proceed" choices can reach it after deciding what to do with the
    /// in-memory edits.
    pub(crate) fn start_git_workspace_save_wizard(&mut self, ci: usize) {
        let Some(origin) = self.collections[ci].workspace_git_origin.clone() else {
            self.status = Some(Status::NoGitOrigin);
            return;
        };
        self.overlay = Some(Overlay::GitSave(Box::new(GitSaveWizard::new_workspace(
            ci,
            &self.collections[ci],
            &origin,
        ))));
    }

    /// Write Workspace tab `ci`'s currently-loaded file back to its path on
    /// disk and clear its "new"/"modified" markers, so a following git push
    /// includes the edits. Returns `false` (setting [`Status::Error`]) if the
    /// file has no path or the write fails, so the caller can abort the push.
    pub(crate) fn save_workspace_current_file(&mut self, ci: usize) -> bool {
        let Some(path) = self.collections[ci].path.clone() else {
            return false;
        };
        let text = self.collections[ci].to_hurl();
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.mark_collection_saved(ci);
                self.save_state();
                true
            }
            Err(e) => {
                self.status = Some(Status::Error(e.to_string()));
                false
            }
        }
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
                    // Only show files worth loading for this kind (a big repo
                    // otherwise buries the one `.hurl`/`.vars` under noise).
                    RemoteStage::PickFile {
                        files: relevant_files(w.kind, &files),
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
                    RemoteKind::Report => {
                        // Build a report straight from the fetched text (keeping
                        // its git provenance so a later "Save to Git" repins
                        // in place), then open it as a new report tab.
                        let fallback = file_stem(&path, "report");
                        let mut report = crate::report::Report::from_text(fallback, text);
                        report.git_origin = origin;
                        self.open_loaded_report(report);
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
                    } else if matches!(w.source, GitSaveSource::Workspace { .. }) {
                        // A Workspace push has no per-file path to choose (the
                        // whole tree is committed as-is), so skip ChoosePaths
                        // and go straight to picking the branch/tag, spawning
                        // the refs fetch as ChoosePaths would have.
                        let url = w.url.text();
                        let token = w.token_opt();
                        w.rx = Some(spawn_git_save_refs(url, token));
                        w.stage = GitSaveStage::ChooseTarget {
                            sel: None,
                            refs: None,
                        };
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
                        let files = match &w.source {
                            GitSaveSource::Workspace { root, .. } => {
                                match crate::workspace::collect_files_for_commit(root) {
                                    Ok(files) if !files.is_empty() => files,
                                    Ok(_) => {
                                        let s = Strings::for_language(&self.language);
                                        w.stage = GitSaveStage::Error(
                                            s.git_save_workspace_empty.to_string(),
                                        );
                                        self.overlay = Some(Overlay::GitSave(w));
                                        return;
                                    }
                                    Err(e) => {
                                        w.stage = GitSaveStage::Error(e.to_string());
                                        self.overlay = Some(Overlay::GitSave(w));
                                        return;
                                    }
                                }
                            }
                            GitSaveSource::Collection => {
                                let col = &self.collections[ci];
                                let mut files = vec![(w.collection_path.text(), col.to_hurl())];
                                if w.include_env
                                    && let Some(env) = w.env.as_ref()
                                {
                                    files.push((w.env_path.text(), env.to_vars_text()));
                                }
                                files
                            }
                            GitSaveSource::Report { report_idx } => {
                                // Push the report's source text as-is to the
                                // chosen path (no accompanying env file).
                                let text = self.reports[*report_idx].report.text.clone();
                                vec![(w.collection_path.text(), text)]
                            }
                        };
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
            GitSaveMsg::Pushed(Ok(new_sha)) => {
                self.finish_git_save(w, &new_sha);
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
    fn finish_git_save(&mut self, w: &GitSaveWizard, new_sha: &str) {
        let ci = w.ci;
        if let GitSaveSource::Report { report_idx } = &w.source {
            // A report push has no per-request markers; just clear the dirty
            // flag and, for a branch target, repin the report's git origin to
            // the path/branch just pushed (a tag save leaves it untouched,
            // mirroring the collection flow).
            let idx = *report_idx;
            if let Some(rt) = self.reports.get_mut(idx) {
                rt.report.dirty = false;
                if w.target_kind == GitSaveTarget::Branch {
                    rt.report.git_origin = Some(GitOrigin {
                        repo_url: w.url.text(),
                        path: w.collection_path.text(),
                        ref_kind: RefKind::Branch,
                        ref_name: w.target_name.text(),
                    });
                }
            }
            self.remember_git_url(&w.url.text());
            self.save_state();
            self.status = Some(Status::GitSaved);
            return;
        }
        if let GitSaveSource::Workspace { filter, .. } = &w.source {
            // A Workspace push commits the on-disk tree, not the in-memory
            // collection, so there are no per-request "modified" markers to
            // clear. For a branch target, repin the remembered origin to the
            // exact commit just pushed so a later redownload fetches it (and
            // follows the same branch); a tag target leaves the origin
            // untouched, mirroring the collection flow.
            if w.target_kind == GitSaveTarget::Branch {
                self.collections[ci].workspace_git_origin = Some(WorkspaceGitOrigin {
                    repo_url: w.url.text(),
                    commit_sha: new_sha.to_string(),
                    ref_kind: RefKind::Branch,
                    ref_name: w.target_name.text(),
                    filter: *filter,
                });
            }
            self.remember_git_url(&w.url.text());
            self.save_state();
            self.status = Some(Status::GitSaved);
            return;
        }
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

/// Display name for an environment file. A leading-dot filename keeps its full
/// name (so `.env.dev-au` stays `.env.dev-au`), and only the known environment
/// extensions (`.env`/`.vars`) are hidden — any other suffix is kept verbatim,
/// so a file like `environment.env.dev-au` shows in full rather than losing its
/// `.dev-au` (the part after the dot is a meaningful suffix here, not a
/// throwaway extension).
pub(crate) fn env_name_from_path(path: &str, fallback: &str) -> String {
    let p = std::path::Path::new(path);
    if let Some(name) = p.file_name()
        && name.to_string_lossy().starts_with('.')
    {
        return name.to_string_lossy().into_owned();
    }
    match p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
    {
        Some(ext) if ext == "env" || ext == "vars" => file_stem(path, fallback),
        _ => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| fallback.to_string()),
    }
}

/// Display name for a collection file. Only the known collection extensions
/// (`.hurl`/`.json`) are hidden; any other suffix is kept verbatim (so a file
/// like `env.dev-au` shows in full rather than losing its `.dev-au`).
pub(crate) fn collection_name_from_path(path: &str, fallback: &str) -> String {
    let p = std::path::Path::new(path);
    match p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
    {
        Some(ext) if ext == "hurl" || ext == "json" => file_stem(path, fallback),
        _ => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| fallback.to_string()),
    }
}

/// Test-only mirror of the pre-`MultiSelectPanel` selection model. The UI
/// tests were written against a flat `text_selection` (the single active
/// region) plus `extra_selections` (the finalized Alt+Click+Drag regions),
/// each tagged with its `Pane`. Selections now live inside each panel
/// (`main_panel` / `resp_panel`), so these helpers translate between the two
/// shapes and keep the large existing test-suite readable.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) struct TextSelection {
    pub(crate) pane: Pane,
    pub(crate) anchor: TextPos,
    pub(crate) cursor: TextPos,
}

#[cfg(test)]
impl TuiApp {
    /// The active selection (whichever body panel currently holds one), in the
    /// old `text_selection` shape. At most one panel is ever active.
    pub(crate) fn text_selection(&self) -> Option<TextSelection> {
        if let Some((anchor, cursor)) = self.main_panel.active_selection() {
            Some(TextSelection {
                pane: Pane::Main,
                anchor,
                cursor,
            })
        } else {
            self.resp_panel
                .active_selection()
                .map(|(anchor, cursor)| TextSelection {
                    pane: Pane::Response,
                    anchor,
                    cursor,
                })
        }
    }

    /// Set (with `Some`) or clear (with `None`) the active selection, mirroring
    /// an assignment to the old `text_selection` field. `None` drops every
    /// region on both panels.
    pub(crate) fn set_text_selection(&mut self, sel: Option<TextSelection>) {
        match sel {
            None => self.clear_selections(),
            Some(s) => match s.pane {
                Pane::Main => self.main_panel.set_active_selection(s.anchor, s.cursor),
                _ => self.resp_panel.set_active_selection(s.anchor, s.cursor),
            },
        }
    }

    /// Every finalized (Alt+Click+Drag) region across both panels, in the old
    /// `extra_selections` shape.
    pub(crate) fn extra_selections(&self) -> Vec<TextSelection> {
        let mut v = Vec::new();
        for (anchor, cursor) in self.main_panel.finalized_selections() {
            v.push(TextSelection {
                pane: Pane::Main,
                anchor,
                cursor,
            });
        }
        for (anchor, cursor) in self.resp_panel.finalized_selections() {
            v.push(TextSelection {
                pane: Pane::Response,
                anchor,
                cursor,
            });
        }
        v
    }

    /// Push a finalized region, mirroring `extra_selections.push(...)`.
    pub(crate) fn push_extra_selection(&mut self, sel: TextSelection) {
        match sel.pane {
            Pane::Main => self.main_panel.push_finalized(sel.anchor, sel.cursor),
            _ => self.resp_panel.push_finalized(sel.anchor, sel.cursor),
        }
    }

    /// Start a drag-autoscroll on a panel, mirroring the old
    /// `pending_autoscroll = Some((pane, dir))` assignment. A negative `dir`
    /// scrolls up, otherwise down.
    pub(crate) fn set_pending_autoscroll(&mut self, pane: Pane, dir: i32) {
        let d = if dir < 0 {
            tui_panel_select::AutoScroll::Up
        } else {
            tui_panel_select::AutoScroll::Down
        };
        match pane {
            Pane::Main => self.main_panel.start_autoscroll(d),
            _ => self.resp_panel.start_autoscroll(d),
        }
    }
}
