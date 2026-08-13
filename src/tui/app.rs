use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui_explorer::FileExplorer;

use crate::collection::Collection;
use crate::environment::{
    EnvVar, Environment, PendingSecret, looks_like_env, parse_vars_pending, spawn_resolution,
};
use crate::git_remote::{self, GitOrigin, RefKind};
use crate::hurl::{FormFieldKind, HurlEntry, RunStatus};
use crate::i18n::{Status, Strings};
use crate::request::{self, build_request_json};

use super::editor::*;
use super::git_save::*;
use super::listscroll::ListScroll;
use super::new_request::*;
use super::postman::*;
use super::remote::*;
use crate::postman_flow::{PostmanEvent, Step};
use crate::postman_import::{ImportFormat, ImportSummary};
use crate::remote_flow::{FlowEvent, RemoteKind, WorkspaceGitFilter, WorkspaceGitOrigin};
use crate::save_flow::{SaveFlow, SaveSource, SaveTargetKind};
use tui_panel_select::MultiSelectPanel;
use tui_panel_select::wrapcache::TextPos;

#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// Picking the PARENT FOLDER the Postman import will create its workspace
    /// folder inside, with the folder's own name typed into the browser's
    /// inline name editor — the same shape as `SaveWorkspaceChooseFolder`. The
    /// wizard is parked in [`TuiApp::parked_postman`] meanwhile, since the
    /// browser needs the overlay slot.
    PostmanDestChooseFolder,
    /// Picking the DESTINATION FOLDER for a workspace file or folder being
    /// moved (see [`TuiApp::pending_workspace_move`]) — confirms on `Space`
    /// like the other folder pickers, since there is no name to type. The
    /// browser is seeded at the workspace root, and the move itself still
    /// refuses anything outside it.
    MoveWorkspaceItemChooseFolder,
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
    /// Picking a DESTINATION FOLDER for "New Report" — creating a brand-new
    /// empty report. Confirms like the other folder pickers (navigate to the
    /// folder, Tab to the filename editor, Enter writes `dir/<name>.trail`);
    /// the filename is seeded with `report.trail`. If the chosen folder lies
    /// inside an open Workspace, the report is created embedded in that
    /// workspace's tree; otherwise it opens as a standalone report tab. `Ctrl+N`
    /// in this browser instead creates an unsaved scratch report tab (no file).
    NewReportChooseFolder,
    /// Picking a SOURCE FOLDER for a `FOR … IN FILES/FOLDERS` node in the
    /// structured report editor. Confirms on `Space` (the current directory,
    /// like `OpenWorkspace`); the chosen path is written into the loop's
    /// producer `dir`. The target node is parked in
    /// [`TuiApp::pending_node_folder`] (`FileAction` stays `Copy`, so the node
    /// path can't live in the variant).
    PickReportNodeFolder,
    /// Picking the FOLDER for a report's `# root:` header directive from the
    /// node editor's settings section. Confirms on `Space` (the current
    /// directory) like the other folder pickers; the target report and
    /// directive are parked in [`TuiApp::pending_header_path`].
    PickReportHeaderFolder,
    /// Picking the FILE for a report's `# baseline:` header directive from the
    /// node editor's settings section — a plain file pick (Enter on the file),
    /// parked the same way as `PickReportHeaderFolder`.
    PickReportHeaderFile,
    /// Picking the FOLDER for a `PARAM … FOLDER` in a report's run settings.
    /// Confirms on `Space` (the current directory) like the other folder
    /// pickers; the target report and parameter are parked in
    /// [`TuiApp::pending_param_path`].
    PickReportParamFolder,
    /// Picking the FILE for a `PARAM … FILE` in a report's run settings — a
    /// plain file pick (Enter on the file), parked the same way as
    /// `PickReportParamFolder`.
    PickReportParamFile,
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
                | FileAction::NewReportChooseFolder
                | FileAction::PostmanDestChooseFolder
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
    /// Naming a new workspace file of any kind, created in the folder the tree
    /// cursor is in (see [`TuiApp::open_new_workspace_item_prompt`]). Which of
    /// the three kinds it is comes from the extension typed, so one key and one
    /// prompt cover all of them rather than a menu asking which first.
    NewWorkspaceItem(usize, std::path::PathBuf),
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
    /// Typing the value of one report-header directive in the node editor's
    /// settings section — `columns:`, a path, or any directive being edited as
    /// raw text with `e`. Committing writes `# <key>: <text>` (an empty commit
    /// removes the directive, matching Delete on the row). Addressed by
    /// `report_id` rather than tab index so a tab reorder can't misroute it,
    /// exactly like [`PromptKind::ReportNodeLine`].
    /// Typing the value of one report `PARAM` in the run settings view.
    /// Committing sets it for the next run only — the source is untouched,
    /// because a parameter's value belongs to the run, not to the report.
    /// Addressed by `report_id` for the same reason as the two above.
    ReportParamValue {
        report_id: u64,
        name: String,
    },
    ReportHeaderValue {
        report_id: u64,
        key: &'static str,
        /// Which occurrence of `key` is being edited — see
        /// [`crate::tui::report_nodes::SettingRow::occurrence`]. `collection:`
        /// repeats, so this is what keeps a helper row from overwriting the
        /// primary collection.
        occurrence: usize,
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
            PromptKind::NewWorkspaceItem(..) => ".hurl",
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

/// Take the overlay out of the app, but **only** when it is the one asked for.
///
/// The obvious spelling of this is a `let ... else` over `self.overlay.take()`,
/// and it is wrong in a way that is easy to miss and severe when it happens:
/// `take()` is evaluated before the pattern is matched, so an overlay that
/// *doesn't* match has already been removed by the time the arm gives up, and
/// is dropped. Anything polled on every pass of the event loop then quietly
/// closes whatever the user just opened — which is exactly how the File and
/// Settings menus, and the quit confirmation, once became unreachable.
///
/// Here the closure is handed the overlay and must give it back (as `Err`) if
/// it isn't interested, so declining to match cannot lose it.
///
/// Prefer the [`take_overlay!`] macro, which writes the give-it-back arm.
macro_rules! take_overlay {
    ($app:expr, $pat:pat => $out:expr) => {
        $app.take_overlay_matching(|overlay| match overlay {
            $pat => Ok($out),
            other => Err(other),
        })
    };
}
pub(crate) use take_overlay;

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
    /// The "import a whole Postman workspace" wizard (File ▸ Load ▸ Workspace ▸
    /// From Postman…). Its state machine is shared with the GUI — see
    /// [`crate::postman_flow`].
    PostmanImport(Box<PostmanWizard>),
    GitSave(Box<GitSaveWizard>),
    /// Dry-run preview for a report tab: the projected row count, a sample of
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
    /// The node editor's **settings** menu ([`Overlay::ReportSettingMenu`]): a
    /// one-step list that either adds a report-header directive (`a` on the
    /// settings section) or picks the value of one that has a closed set of
    /// answers — the output format, or one of the loaded environments. The
    /// directives whose values are open-ended use a text prompt instead, and
    /// `collection:` reuses the existing bind picker.
    /// See [`crate::tui::report_nodes::SettingMenu`].
    ReportSettingMenu(Box<crate::tui::report_nodes::SettingMenu>),
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
    /// The structured node editor's `FOR … IN FILES` configure form
    /// ([`Overlay::ReportNodeFiles`]): picks the loop variable, the source
    /// folder (via the file picker), an optional `MATCH` glob and the
    /// `PARALLEL` toggle. Opened with `Enter` on a `FOR … IN FILES` node. See
    /// [`crate::tui::report_nodes::FilesForm`].
    ReportNodeFiles(Box<crate::tui::report_nodes::FilesForm>),
    /// The structured node editor's `WITH name: query` field form
    /// ([`Overlay::ReportNodeWithField`]): one ad-hoc column of a report
    /// request's `WITH … END` block — its name, its Hurl query and its
    /// `STATISTICS(…)` checklist. Opened from a `WITH` row of the request form.
    /// See [`crate::tui::report_nodes::WithFieldForm`].
    ReportNodeWithField(Box<crate::tui::report_nodes::WithFieldForm>),
    /// The structured node editor's `VARIABLE = VALUE` form
    /// ([`Overlay::ReportNodeAssign`]). See
    /// [`crate::tui::report_nodes::AssignForm`].
    ReportNodeAssign(Box<crate::tui::report_nodes::AssignForm>),
    /// The structured node editor's `LIST NAME = [ … ]` form
    /// ([`Overlay::ReportNodeList`]). See
    /// [`crate::tui::report_nodes::ListForm`].
    ReportNodeList(Box<crate::tui::report_nodes::ListForm>),
    /// The structured node editor's `REPORT <var>` form
    /// ([`Overlay::ReportNodeVars`]): which in-scope variables become columns,
    /// and for a single one its `AS` name and `STATISTICS(…)`. See
    /// [`crate::tui::report_nodes::VarsForm`].
    ReportNodeVars(Box<crate::tui::report_nodes::VarsForm>),
    /// The structured node editor's `REPORT "<template>" AS <name>` form
    /// ([`Overlay::ReportNodeComputed`]). See
    /// [`crate::tui::report_nodes::ComputedForm`].
    ReportNodeComputed(Box<crate::tui::report_nodes::ComputedForm>),
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
/// Not `Copy`: [`Self::RevertWorkspaceFile`] names a file, and a path can't be
/// squeezed into an index the way the other variants' targets can (a workspace
/// tree row is a file, not a slot in a list).
#[derive(Clone, PartialEq)]
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
    /// Discard every in-memory edit to a workspace collection file, restoring
    /// it from disk. Holds `(tab index, file path)`. Raised by a right-click on
    /// an edited file row in the workspace tree (and by the GUI's context
    /// menu), only when that file actually has unsaved edits.
    RevertWorkspaceFile(usize, std::path::PathBuf),
    /// Rerun the active report when its current on-screen results haven't been
    /// exported (CSV/JSON/HTML/XLSX or a `.baseline` snapshot) since the run
    /// that produced them — confirming discards the unsaved results. Acts on the
    /// active report tab. Raised only when [`ReportTab::results_exported`] is
    /// false and no run is in flight (#2).
    RerunReport,
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

/// Interaction layers in paint order. Mouse dispatch only considers hits from
/// the topmost layer drawn in the last frame, so overlays never leak clicks to
/// the UI underneath them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum MouseLayer {
    Base,
    Overlay,
    Popup,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MouseScrollTarget {
    List,
    GlobalEnv,
    Main,
    Response,
    ReportPane(crate::tui::reports::ReportPane),
    Help,
    ReportCellPopup,
    WizardBody,
    WizardAllSections,
    WizardKvd(KvdKind),
    WizardForm,
    WizardAsserts,
    WizardCaptures,
    WizardReports,
    OverlayList,
    BrowserList,
    WorkspacePicker,
    ThemeEditor,
    RemoteWizard,
    GitSaveWizard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum WizardDropdownKind {
    HeaderName,
    FormKind,
    ContentType,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MouseHitTarget {
    MenuFile,
    MenuSettings,
    Tab(usize),
    FocusPane(Pane),
    SelectListRow(usize),
    SelectGlobalEnvRow(usize),
    RunRequest,
    ReportResultsCell,
    ReportNodeRow(usize),
    /// A row of the node editor's report-settings section, above the outline.
    ReportSettingRow(usize),
    OverlayRow(usize),
    ConfirmChoice(usize),
    HelpTab(usize),
    PromptEditor,
    PromptSecretCheckbox,
    EnvVarField(bool),
    BrowserListRow(usize),
    BrowserNameField,
    WorkspacePickerRow(usize),
    NewRequestField(NewField),
    NewRequestActivate(NewField),
    NewRequestTab(WizardTab),
    NewRequestDropdown(WizardDropdownKind, usize),
    ThemeEditorRow(usize),
    ThemeEditorColor(usize),
    ThemeEditorColorChoice(usize),
    RemoteWizardRow(usize),
    GitSaveWizardRow(usize),
    Scroll(MouseScrollTarget),
}

impl MouseHitTarget {
    pub(crate) fn scroll_target(self) -> Option<MouseScrollTarget> {
        match self {
            MouseHitTarget::FocusPane(Pane::List) | MouseHitTarget::SelectListRow(_) => {
                Some(MouseScrollTarget::List)
            }
            MouseHitTarget::FocusPane(Pane::GlobalEnv) | MouseHitTarget::SelectGlobalEnvRow(_) => {
                Some(MouseScrollTarget::GlobalEnv)
            }
            MouseHitTarget::FocusPane(Pane::Main) => Some(MouseScrollTarget::Main),
            MouseHitTarget::FocusPane(Pane::Response) => Some(MouseScrollTarget::Response),
            MouseHitTarget::ReportResultsCell => Some(MouseScrollTarget::ReportPane(
                crate::tui::reports::ReportPane::Results,
            )),
            MouseHitTarget::ReportNodeRow(_) | MouseHitTarget::ReportSettingRow(_) => Some(
                MouseScrollTarget::ReportPane(crate::tui::reports::ReportPane::Source),
            ),
            MouseHitTarget::OverlayRow(_) | MouseHitTarget::ConfirmChoice(_) => {
                Some(MouseScrollTarget::OverlayList)
            }
            MouseHitTarget::BrowserListRow(_) => Some(MouseScrollTarget::BrowserList),
            MouseHitTarget::WorkspacePickerRow(_) => Some(MouseScrollTarget::WorkspacePicker),
            MouseHitTarget::NewRequestField(NewField::Body) => Some(MouseScrollTarget::WizardBody),
            MouseHitTarget::NewRequestField(NewField::Kvd(kind, ..))
            | MouseHitTarget::NewRequestActivate(NewField::AddKvd(kind)) => {
                Some(MouseScrollTarget::WizardKvd(kind))
            }
            MouseHitTarget::NewRequestField(NewField::FormField(..))
            | MouseHitTarget::NewRequestActivate(NewField::AddFormField) => {
                Some(MouseScrollTarget::WizardForm)
            }
            MouseHitTarget::NewRequestField(NewField::Assert(_))
            | MouseHitTarget::NewRequestActivate(NewField::AddAssert) => {
                Some(MouseScrollTarget::WizardAsserts)
            }
            MouseHitTarget::NewRequestField(NewField::Capture(..))
            | MouseHitTarget::NewRequestActivate(NewField::AddCapture) => {
                Some(MouseScrollTarget::WizardCaptures)
            }
            MouseHitTarget::NewRequestField(NewField::Report(..))
            | MouseHitTarget::NewRequestActivate(NewField::AddReport) => {
                Some(MouseScrollTarget::WizardReports)
            }
            MouseHitTarget::ThemeEditorRow(_) | MouseHitTarget::ThemeEditorColor(_) => {
                Some(MouseScrollTarget::ThemeEditor)
            }
            MouseHitTarget::RemoteWizardRow(_) => Some(MouseScrollTarget::RemoteWizard),
            MouseHitTarget::GitSaveWizardRow(_) => Some(MouseScrollTarget::GitSaveWizard),
            MouseHitTarget::Scroll(target) => Some(target),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MouseHit {
    pub(crate) rect: Rect,
    pub(crate) layer: MouseLayer,
    pub(crate) target: MouseHitTarget,
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

/// The "Load" source choices for `kind`: a local file or a git remote, plus —
/// for a Workspace — bulk import from Postman, which produces a whole folder
/// of collections and environments and so has nowhere else to belong.
pub(crate) fn file_load_source_items(kind: FileKind, s: &Strings) -> Vec<&'static str> {
    match kind {
        FileKind::Workspace => vec![
            s.file_source_local,
            s.file_source_git,
            s.file_source_postman,
        ],
        _ => vec![s.file_source_local, s.file_source_git],
    }
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
    /// Undo stack for deleted Global Environments: each entry is the list index
    /// the environment was removed from plus the environment itself, so `u`
    /// (in the Global Environments panel) can reopen the most recent one. The
    /// exact parallel of a collection's `deleted_entries`.
    pub(crate) deleted_envs: Vec<(usize, Environment)>,

    pub(crate) focus: Pane,
    /// Selected row in the Global Environments list — an index into
    /// [`Self::env_rows`], *not* into `global_envs`: the panel also lists the
    /// open Workspace's environment files, including ones not loaded yet, and
    /// the filter can hide any of them. Use [`Self::selected_env_row`] to get
    /// at what it points to.
    pub(crate) global_env_idx: usize,
    /// Type-to-filter query for the Global Environments panel (`/` starts it).
    /// A case-insensitive substring of the environment name; empty means no
    /// filtering. Runtime-only — a filter is a way of finding something now,
    /// not a setting, so it isn't persisted across restarts.
    pub(crate) env_query: String,
    /// True while `/` filter entry is capturing keys for the Global
    /// Environments panel, so letters type into the query instead of firing the
    /// panel's single-key actions (`a`, `x`, `u`, …).
    pub(crate) env_filter_typing: bool,
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
    /// When true, the Response body view shortens long string literals to a
    /// `"head...tail"` overview (see
    /// [`crate::shared_utils::compact_long_strings`]). Toggled with `c` while
    /// the Response pane is focused. Display-only: `resp_full_body` keeps the
    /// untruncated text so a whole-panel `y`-copy still yields the full body.
    pub(crate) response_compact: bool,
    /// The full (untruncated) Response body cached each frame the normal body is
    /// drawn, so the whole-panel copy fallback can return it even while the
    /// panel is showing the compacted overview. Empty when the current frame
    /// isn't showing a compactable body (loading / no-response / error).
    pub(crate) resp_full_body: Arc<str>,
    /// Per-line column map from the *compacted* Response body back to the full
    /// body, rebuilt alongside `resp_full_body` each frame the compact overview
    /// is drawn. Lets a drag-selection over compacted text copy the untruncated
    /// value (see `resp_full_selected_parts`). Empty when not compacting.
    pub(crate) resp_compact_line_maps: Vec<Vec<usize>>,
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
    /// Whether the pointer has actually moved since the left button went down
    /// over a text panel. `begin` on a panel always leaves a degenerate
    /// one-character region behind, so "is anything selected?" cannot tell a
    /// deliberate drag from a plain click — and a plain click must not copy
    /// (it is how a panel is focused). Set by the `Drag` handlers, cleared on
    /// `Down`, read on `Up`. Runtime-only.
    pub(crate) mouse_drag_moved: bool,
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
    /// The request/workspace tree's vertical scroll offset, carried between
    /// frames and tagged with the tab it belongs to (another tab's scroll
    /// position means nothing in this one). See [`ListScroll`] for why a list
    /// that forgets where it was scrolled to follows the cursor instead of
    /// letting the cursor move through it.
    pub(crate) list_scroll: ListScroll,
    /// Scroll position of the Global Environments panel.
    pub(crate) env_list_scroll: ListScroll,
    /// Scroll position of the variables list inside the environment popup,
    /// tagged with the environment on show so opening a different one starts
    /// at the top.
    pub(crate) env_var_scroll: ListScroll,
    /// Scroll position of the workspace file picker, tagged with the folder it
    /// is listing.
    pub(crate) ws_picker_scroll: ListScroll,
    pub(crate) global_env_scroll_w: std::cell::Cell<u16>,
    pub(crate) overlay: Option<Overlay>,
    /// Vertical scroll offset (rows) into the currently-open Help popup's
    /// body — reset to 0 whenever Help is (re)opened or its tab is
    /// switched. Lets a Help body taller than the terminal be scrolled with
    /// Up/Down instead of those keys just closing the popup.
    pub(crate) help_scroll: u16,
    /// Case-insensitive substring filter typed into the open Help popup, or
    /// empty for "show everything". Applied to every tab's entries (matched
    /// against both the key/label column and the description) so a user can
    /// type part of a shortcut or word to narrow a long reference down to the
    /// lines that mention it. Reset when Help is (re)opened; the first Esc
    /// clears it and a second Esc then closes Help.
    pub(crate) help_query: String,
    pub(crate) quit: bool,

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

    /// One-shot start folder for the *new-report* folder browser
    /// ([`FileAction::NewReportChooseFolder`]): the highlighted workspace folder
    /// (or the workspace root) the "New Report" action was launched from.
    /// Consumed and cleared by [`TuiApp::open_browser`]; falls back to
    /// `last_browse_dir` when unset.
    pub(crate) new_report_seed_dir: Option<PathBuf>,

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
    /// The Postman import wizard, parked while its destination folder is picked
    /// in the browser. Restored on both confirm and cancel, so browsing never
    /// costs the key and options already typed.
    pub(crate) parked_postman: Option<Box<PostmanWizard>>,
    /// Where the destination picker should open, set just before it is.
    pub(crate) postman_dest_seed_dir: Option<std::path::PathBuf>,
    /// The target `FOR … IN FILES/FOLDERS` node whose source folder is being
    /// chosen while a [`FileAction::PickReportNodeFolder`] browser is open:
    /// `(report id, node path)`. The chosen directory is written into that
    /// loop's producer `dir` on `Space`. Runtime-only (not persisted).
    pub(crate) pending_node_folder: Option<(u64, Vec<usize>)>,
    /// The report id and header-directive key a `root:` / `baseline:` file
    /// browser is picking for, parked while the browser owns the overlay slot
    /// (`FileAction` is `Copy`, so the key can't live in the variant — the same
    /// reason [`TuiApp::pending_node_folder`] exists).
    pub(crate) pending_header_path: Option<(u64, &'static str)>,
    /// The report and parameter a `PickReportParam*` browser is picking for,
    /// parked here because `FileAction` has no room for a name. Cleared when
    /// the pick is committed or abandoned.
    pub(crate) pending_param_path: Option<(u64, String)>,
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
    /// Type-to-filter query for the local load browser (Open Collection / Load
    /// Environment / Open Report): printable keys accumulate here and narrow the
    /// file list to names containing this substring (case-insensitive), on top
    /// of the extension filter. Backspace trims it, Esc clears it (then cancels
    /// on a second press), and it resets whenever the browser opens. Runtime-only.
    pub(crate) browser_query: String,
    /// The `label  ·  keys` line drawn along the bottom border of the file
    /// browser. Built once in `open_browser` (where the action's label and hint
    /// are chosen) and baked into the explorer's own theme — kept here as well
    /// so the "no matches" placeholder, which replaces the explorer widget
    /// entirely when a filter empties the list, can draw the same frame instead
    /// of a bare box that loses the keys just when they're most needed.
    pub(crate) browser_hint_line: String,
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
    /// The workspace file or folder parked for a move, awaiting a destination
    /// folder chosen through the browser (see
    /// [`FileAction::MoveWorkspaceItemChooseFolder`]). Carries the tab it came
    /// from so the tree can be re-focused on the item once it has moved.
    pub(crate) pending_workspace_move: Option<(usize, std::path::PathBuf)>,

    /// Last row clicked by the mouse. Runtime-only: keyboard input, wheel
    /// input, or any non-row mouse-down breaks the consecutive-click pair.
    pub(crate) last_mouse_row: Option<MouseHitTarget>,
    pub(crate) mouse_hits: RefCell<Vec<MouseHit>>,
    pub(crate) mouse_top_layer: Cell<MouseLayer>,
    pub(crate) mouse_hit_valid: Cell<bool>,

    /// The front-end-agnostic application state: collections, environments,
    /// themes, preferences and the persisted settings both front-ends share.
    /// `TuiApp` derefs to it, so `self.collections` still reaches the one copy
    /// that gets written to `state.json` — the terminal UI keeps only *view*
    /// state (cursors, scroll offsets, overlays, focus, wrap caches) of its own.
    pub(crate) session: crate::session::Session,
}

/// `TuiApp` owns the shared [`Session`] and reaches straight through it, so
/// every existing `self.collections` / `self.status` / `self.list_width` call
/// site keeps working while there is only one copy of that state in the process
/// — and therefore only one writer of `state.json`.
///
/// A `Deref` rather than a wall of accessors because the alternative is ~2,200
/// mechanical rewrites for no behavioural gain. The cost is that a method
/// borrowing a session field and a view field at once now borrows all of
/// `self`; where that bites, the fix is to name `self.session.<field>` and
/// `self.<view field>` explicitly, which the borrow checker treats as disjoint.
impl std::ops::Deref for TuiApp {
    type Target = crate::session::Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl std::ops::DerefMut for TuiApp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            reports: Vec::new(),
            deleted_envs: Vec::new(),
            focus: Pane::List,
            global_env_idx: 0,
            env_query: String::new(),
            env_filter_typing: false,
            resp_max_scroll: 0,
            main_max_scroll: 0,
            list_hscroll: 0,
            global_env_hscroll: 0,
            main_text_area: Rect::default(),
            main_panel: MultiSelectPanel::new(),
            main_shadow_icon_positions: std::collections::HashSet::new(),
            resp_text_area: Rect::default(),
            resp_panel: MultiSelectPanel::new(),
            response_compact: false,
            resp_full_body: Arc::from(""),
            resp_compact_line_maps: Vec::new(),
            main_scrollbar_area: Rect::default(),
            resp_scrollbar_area: Rect::default(),
            scrollbar_drag: None,
            mouse_drag_moved: false,
            report_pane_areas: [Rect::default(); 3],
            report_pane_bars: [Rect::default(); 3],
            report_scrollbar_drag: None,
            prompt_editor_area: Rect::default(),
            list_scroll_w: std::cell::Cell::new(0),
            list_scroll: ListScroll::default(),
            env_list_scroll: ListScroll::default(),
            env_var_scroll: ListScroll::default(),
            ws_picker_scroll: ListScroll::default(),
            global_env_scroll_w: std::cell::Cell::new(0),
            overlay: None,
            help_scroll: 0,
            help_query: String::new(),
            quit: false,
            pending_report_runs: Vec::new(),
            running_reports: std::collections::HashMap::new(),
            new_report_seed_dir: None,
            browser_origin_dir: None,
            browser_forward_path: None,
            enhanced_keys: false,
            pending_save_path: None,
            pending_workspace_request: None,
            pending_workspace_transfer: None,
            closed_tabs: Vec::new(),
            parked_wizard: None,
            parked_postman: None,
            postman_dest_seed_dir: None,
            pending_node_folder: None,
            pending_header_path: None,
            pending_param_path: None,
            browser_name: Editor::new("", false),
            browser_name_focused: false,
            browser_filter_on: true,
            browser_query: String::new(),
            browser_hint_line: String::new(),
            wizard_return_focus: Pane::List,
            pending_collision_env: None,
            workspace_redownload_rx: None,
            pending_workspace_save: None,
            pending_workspace_move: None,
            last_mouse_row: None,
            mouse_hits: RefCell::new(Vec::new()),
            mouse_top_layer: Cell::new(MouseLayer::Base),
            mouse_hit_valid: Cell::new(false),
            session: crate::session::Session::default(),
        }
    }
}

impl TuiApp {
    /// The safe half of [`take_overlay!`]: hand the open overlay to `f`, which
    /// either extracts what it wants from it or gives it back untouched.
    ///
    /// Returning the overlay in the `Err` case is what makes this safe — a
    /// caller that isn't interested cannot accidentally drop what was open.
    pub(crate) fn take_overlay_matching<T>(
        &mut self,
        f: impl FnOnce(Overlay) -> Result<T, Overlay>,
    ) -> Option<T> {
        match f(self.overlay.take()?) {
            Ok(taken) => Some(taken),
            Err(put_back) => {
                self.overlay = Some(put_back);
                None
            }
        }
    }

    pub(crate) fn begin_mouse_frame(&self) {
        self.mouse_hits.borrow_mut().clear();
        self.mouse_top_layer.set(MouseLayer::Base);
        self.mouse_hit_valid.set(true);
    }

    pub(crate) fn invalidate_mouse_hits(&self) {
        self.mouse_hits.borrow_mut().clear();
        self.mouse_top_layer.set(MouseLayer::Base);
        self.mouse_hit_valid.set(false);
    }

    pub(crate) fn set_mouse_layer(&self, layer: MouseLayer) {
        if layer > self.mouse_top_layer.get() {
            self.mouse_top_layer.set(layer);
        }
    }

    pub(crate) fn push_mouse_hit(&self, layer: MouseLayer, rect: Rect, target: MouseHitTarget) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.set_mouse_layer(layer);
        let mut hits = self.mouse_hits.borrow_mut();
        hits.push(MouseHit {
            rect,
            layer,
            target,
        });
    }

    pub(crate) fn mouse_hit_at(&self, point: ratatui::layout::Position) -> Option<MouseHitTarget> {
        if !self.mouse_hit_valid.get() {
            return None;
        }
        let layer = self.mouse_top_layer.get();
        self.mouse_hits
            .borrow()
            .iter()
            .rev()
            .find(|hit| hit.layer == layer && hit.rect.contains(point))
            .map(|hit| hit.target)
    }

    pub(crate) fn mouse_scroll_target_at(
        &self,
        point: ratatui::layout::Position,
    ) -> Option<MouseScrollTarget> {
        if !self.mouse_hit_valid.get() {
            return None;
        }
        let layer = self.mouse_top_layer.get();
        self.mouse_hits
            .borrow()
            .iter()
            .rev()
            .find(|hit| hit.layer == layer && hit.rect.contains(point))
            .and_then(|hit| hit.target.scroll_target())
    }

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
        // Mark just this entry as in-flight so the Response pane shows the
        // spinner only while *this* selected entry is sending — selecting a
        // different entry mid-send now shows that entry's own last response
        // rather than a blanket "Sending…".
        let selected = self.collections[col_idx].selected_entry;
        if let Some(entry) = self.collections[col_idx].entries.get_mut(selected) {
            entry.last_run = RunStatus::Running;
        }
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
        // Named through `session` because three simultaneous `&mut` borrows of
        // distinct fields are only disjoint when the compiler can see the
        // fields; going through `DerefMut` would borrow all of `self` thrice.
        let s = &mut self.session;
        request::drain_env_updates(&mut s.pending_env, &mut s.global_envs, &mut s.collections);
    }

    /// Drain completed response captures into their collections so subsequent
    /// requests can substitute the captured values.
    pub(crate) fn poll_capture_updates(&mut self) {
        let s = &mut self.session;
        request::drain_capture_updates(&mut s.pending_captures, &mut s.collections);
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
            let mut run_col_id: Option<u64> = None;
            loop {
                match rx.try_recv() {
                    Ok(update) => {
                        run_col_id = Some(update.col_id);
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
                                    // The runner hasn't reached this entry *yet*
                                    // in this streaming pass — it's still in
                                    // flight, so keep it "Running" (the Response
                                    // pane shows the spinner only for Running
                                    // entries). Unreached-because-stopped-early
                                    // is reset to NotRun once the run ends (below).
                                    None => RunStatus::Running,
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
            // The run has ended: any entry still marked "Running" was never
            // reached (the run stopped early / the collection failed to parse),
            // so drop it back to "hasn't run" rather than leaving it spinning.
            if disconnected
                && let Some(col_id) = run_col_id
                && let Some(col) = self.collections.iter_mut().find(|c| c.id == col_id)
            {
                for entry in col.entries.iter_mut() {
                    if entry.last_run == RunStatus::Running {
                        entry.last_run = RunStatus::NotRun;
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
            PromptKind::NewWorkspaceItem(ci, dir) => self.create_workspace_item(ci, &dir, text),
            PromptKind::ReportNodeLine { report_id, path } => {
                self.commit_report_node_line(report_id, &path, text)
            }
            PromptKind::ReportParamValue { report_id, name } => {
                self.set_report_param(report_id, &name, text);
            }
            PromptKind::ReportHeaderValue {
                report_id,
                key,
                occurrence,
            } => {
                if let Some(idx) = self.report_index_by_id(report_id) {
                    // An empty commit means "remove it", the same as Delete on
                    // the row — otherwise clearing the field would write back
                    // the `?` placeholder and look like nothing happened.
                    let text = text.trim().to_string();
                    let value = (!text.is_empty()).then_some(text);
                    self.apply_report_setting(idx, key, occurrence, value.as_deref());
                }
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
    /// How many requests across every tab would lose their edits to a quit.
    /// Drives the exit warning.
    ///
    /// Only Workspace tabs count: every other tab's entries are written to the
    /// session state as they stand, so its edits are waiting — still marked as
    /// edited — at the next start (see `Collection::edits_lost_on_exit`).
    pub(crate) fn unsaved_request_edits(&self) -> usize {
        self.collections
            .iter()
            .map(|c| c.edits_lost_on_exit())
            .sum()
    }

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
        self.collections[ci].mark_saved();
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
        self.collections.get_mut(ci)?.revert_request(ei)
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
                // Refuse to write a file PaperBoy couldn't read back: a file
                // field with no path serializes to an invalid `file,;` line.
                if let Some((req, field)) = self.collections[ci].first_empty_file_field() {
                    self.status = Some(Status::SaveUnreadableEmptyFile { req, field });
                    return;
                }
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
            // Folder-only like `OpenWorkspace`: `Space` confirms the current
            // folder as the move destination (handled in `input.rs`), so no
            // path ever arrives here.
            FileAction::MoveWorkspaceItemChooseFolder => {}
            // Folder-only like the pickers above: the Postman destination is
            // confirmed with `Space`, or with `Enter` on the inline folder-name
            // field, both of which route through `finish_postman_dest`.
            FileAction::PostmanDestChooseFolder => {}
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
            // Like the folder pickers above: the new-report destination is
            // confirmed in `input.rs` (`browser_commit_save`), which routes
            // creation through `create_report_at_path`, so this never reaches
            // here.
            FileAction::NewReportChooseFolder => {}
            // Like the folder pickers above: the loop's source folder is
            // confirmed with `Space` in `input.rs`
            // (`commit_report_node_folder`), so a file-Enter never reaches here.
            FileAction::PickReportNodeFolder => {}
            // Like the loop's source folder: `root:` is confirmed with `Space`
            // in `input.rs`, so a file-Enter never reaches here.
            FileAction::PickReportHeaderFolder => {}
            // `baseline:` is a *file* pick, so Enter on the file does land here.
            FileAction::PickReportHeaderFile => self.commit_report_header_path(path),
            // Same split as the header pickers: the folder is confirmed with
            // `Space` in `input.rs`, only the file lands here.
            FileAction::PickReportParamFolder => {}
            FileAction::PickReportParamFile => self.commit_report_param_path(path),
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
            // Prefer the concrete Hurl parse reason (line + what's wrong) over
            // the generic "no requests found": a single malformed line — e.g. a
            // `[Multipart]` `file,;` with an empty filename — makes `hurl_core`
            // reject the *entire* file, so pointing at the offending line is far
            // more actionable. We only do this for Hurl source, not a failed
            // Postman import, where a Hurl-parse reason would be nonsense.
            let s = Strings::for_language(&self.language);
            let reason = if crate::postman::looks_like_postman(content) {
                None
            } else {
                crate::hurl::parse_hurl_error(content)
            };
            self.status = Some(match reason {
                Some(why) => Status::Error(format!("{} {why}", s.file_not_collection_prefix)),
                None => Status::NotCollection,
            });
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
        let Some(col) = self.collections.get_mut(collection_idx) else {
            return;
        };
        // The read+parse+tree-sync is shared with the GUI (see
        // `Collection::load_workspace_file`); the terminal UI only adds its own
        // focus/status/persistence handling around it.
        match col.load_workspace_file(path) {
            Ok(()) => {
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
        // Refuse to write a request PaperBoy couldn't read back: a file field
        // with no path serializes to an invalid `file,;` line.
        if let Some(field) = entry.first_empty_file_field() {
            self.status = Some(Status::SaveUnreadableEmptyFile {
                req: entry.title.clone(),
                field: field.to_string(),
            });
            return;
        }
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

    /// Park the highlighted workspace file or folder for a move and open the
    /// destination-folder browser.
    ///
    /// Requests are not files, so they are left to the existing `m`/`c`
    /// transfer, which moves a request *between* collections rather than moving
    /// a file on disk.
    pub(crate) fn start_workspace_item_move(&mut self) {
        let ci = self.active_tab;
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        let Some(row) = col.ws_rows().into_iter().nth(col.list_cursor) else {
            return;
        };
        if matches!(row, crate::collection::WsRow::Request { .. }) {
            return;
        }
        self.pending_workspace_move = Some((ci, row.path().to_path_buf()));
        self.open_browser(FileAction::MoveWorkspaceItemChooseFolder);
        // Start at the workspace root: the destination has to be inside it, so
        // wherever the browser was last is rarely the right place to begin.
        if let Some(Overlay::Browser(_, ex)) = self.overlay.as_mut() {
            let _ = ex.set_cwd(&root);
        }
    }

    /// Move the parked workspace item into `dest_dir`, then re-point everything
    /// that was holding it and put the tree cursor back on it.
    ///
    /// Shares [`crate::workspace::move_item`] with the graphical front-end, so
    /// both refuse to leave the workspace, to overwrite, or to move a folder
    /// inside itself in exactly the same way.
    pub(crate) fn finish_workspace_item_move(&mut self, dest_dir: std::path::PathBuf) {
        use crate::workspace::{MoveError, move_item, repoint};

        let Some((ci, src)) = self.pending_workspace_move.take() else {
            return;
        };
        let Some(root) = self
            .collections
            .get(ci)
            .and_then(|c| c.workspace_root.clone())
        else {
            return;
        };
        let dest = match move_item(&root, &src, &dest_dir) {
            Ok(dest) => dest,
            Err(MoveError::Exists(what)) => {
                self.status = Some(Status::WsItemMoveExists(what));
                return;
            }
            Err(MoveError::IntoItself) => {
                self.status = Some(Status::WsItemMoveIntoItself);
                return;
            }
            Err(MoveError::Escapes(what)) => {
                self.status = Some(Status::WsItemEscaped(what));
                return;
            }
            Err(MoveError::Io(what)) => {
                self.status = Some(Status::Error(what));
                return;
            }
        };
        if dest == src {
            return;
        }

        for col in &mut self.collections {
            if let Some(p) = col.path.clone().and_then(|p| repoint(&p, &src, &dest)) {
                col.path = Some(p);
            }
            if let Some(p) = col
                .workspace_selected
                .clone()
                .and_then(|p| repoint(&p, &src, &dest))
            {
                col.workspace_selected = Some(p);
            }
            col.workspace_expanded = col
                .workspace_expanded
                .iter()
                .map(|p| repoint(p, &src, &dest).unwrap_or_else(|| p.clone()))
                .collect();
            col.workspace_titles = col
                .workspace_titles
                .drain()
                .map(|(p, v)| (repoint(&p, &src, &dest).unwrap_or(p), v))
                .collect();
        }
        // An embedded report showing the moved file must follow it too, or
        // Ctrl+S would write it back to where it used to be.
        for rt in &mut self.reports {
            if let Some(p) = rt
                .report
                .path
                .clone()
                .and_then(|p| repoint(&p, &src, &dest))
            {
                rt.report.path = Some(p);
            }
        }

        self.reveal_workspace_path(ci, &dest);
        self.save_state();
        self.status = Some(Status::WsItemMoved(crate::workspace::display_name(
            &root, &dest,
        )));
    }

    /// Open the "name a new workspace file" prompt for Workspace tab `ci`,
    /// creating it in `dir` (the folder the tree cursor is sitting in).
    ///
    /// One prompt for all three kinds: what gets made is decided by the
    /// extension typed, which is already how the tree tells a collection from a
    /// report from an environment. That keeps this to a single keystroke
    /// instead of a menu asking which kind before asking for a name.
    pub(crate) fn open_new_workspace_item_prompt(&mut self, ci: usize) {
        let Some(col) = self.collections.get(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        // New files land beside whatever is highlighted — in that folder for a
        // folder row, next to it for a file — which is almost always where the
        // user is looking.
        let dir = match col.ws_rows().into_iter().nth(col.list_cursor) {
            Some(crate::collection::WsRow::Folder { path, .. }) => path,
            Some(row) => row.path().parent().unwrap_or(&root).to_path_buf(),
            None => root.clone(),
        };
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::NewWorkspaceItem(ci, dir),
            editor: Editor::blank(),
            title: s.workspace_new_item_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Create the named collection / report / environment inside `dir` and show
    /// it. Mirrors the graphical front-end's New menu, sharing the same
    /// [`crate::workspace::create_item`] so both obey the same containment and
    /// no-overwrite rules.
    pub(crate) fn create_workspace_item(&mut self, ci: usize, dir: &std::path::Path, name: String) {
        use crate::workspace::{NewItemError, NewItemKind};

        let Some(root) = self
            .collections
            .get(ci)
            .and_then(|c| c.workspace_root.clone())
        else {
            return;
        };
        let Some(kind) = NewItemKind::from_name(name.trim()) else {
            self.status = Some(Status::WsItemUnknownKind(name));
            return;
        };
        let path = match crate::workspace::create_item(&root, dir, &name, kind) {
            Ok(path) => path,
            Err(NewItemError::EmptyName) => return,
            Err(NewItemError::Escapes(what)) => {
                self.status = Some(Status::WsItemEscaped(what));
                return;
            }
            Err(NewItemError::Exists(what)) => {
                self.status = Some(Status::WsItemExists(what));
                return;
            }
            Err(NewItemError::Io(what)) => {
                self.status = Some(Status::Error(what));
                return;
            }
        };

        self.reveal_workspace_path(ci, &path);
        // A new report opens in the tab's report pane, exactly as `R` does; the
        // other two are files the tree now lists, and a collection additionally
        // becomes the loaded one so requests can be added to it straight away.
        match kind {
            NewItemKind::Report => {
                self.show_embedded_report(path.clone(), root.clone());
            }
            NewItemKind::Collection => {
                self.load_workspace_file(ci, path.clone());
            }
            NewItemKind::Environment => {}
            // Nothing to open: a new folder is somewhere to put files, and the
            // tree already lists it (revealed above).
            NewItemKind::Folder => {}
        }
        self.save_state();
        self.status = Some(Status::WsItemCreated(crate::workspace::display_name(
            &root, &path,
        )));
    }

    /// Expand every folder between the workspace root and `path`, then put the
    /// tree cursor on its row — so a file that was just created or moved is
    /// both visible and selected rather than hidden in a collapsed folder.
    pub(crate) fn reveal_workspace_path(&mut self, ci: usize, path: &std::path::Path) {
        let Some(col) = self.collections.get_mut(ci) else {
            return;
        };
        let Some(root) = col.workspace_root.clone() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Ok(rel) = parent.strip_prefix(&root)
        {
            let mut cur = root.clone();
            col.workspace_expanded.insert(cur.clone());
            for component in rel.components() {
                cur.push(component);
                col.workspace_expanded.insert(cur.clone());
            }
        }
        if let Some(i) = col.ws_rows().iter().position(|r| r.path() == path) {
            col.list_cursor = i;
        }
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

    /// The Environments panel's rows: the open Workspace's environment files
    /// (loaded or not) followed by every other loaded environment, narrowed by
    /// the `/` filter query. See [`crate::env_panel`].
    ///
    /// Recomputed on demand rather than cached, exactly as the Workspace tree's
    /// [`crate::collection::Collection::ws_rows`] is — the folder scan is the
    /// same one, and a cache would have to be invalidated on every file
    /// created, moved or deleted from anywhere in the app.
    pub(crate) fn env_rows(&self) -> Vec<crate::env_panel::EnvRow> {
        let files = self.workspace_env_files();
        crate::env_panel::rows(
            &self.global_envs,
            &files,
            &self.env_query,
            self.effective_env_source(),
        )
    }

    pub(crate) fn workspace_env_files(&self) -> Vec<std::path::PathBuf> {
        self.collections
            .get(self.active_tab)
            .map(|c| c.workspace_env_files())
            .unwrap_or_default()
    }

    pub(crate) fn has_workspace_env_source(&self) -> bool {
        self.collections
            .get(self.active_tab)
            .and_then(|c| c.workspace_root.as_deref())
            .is_some()
    }

    pub(crate) fn effective_env_source(&self) -> crate::env_panel::EnvSource {
        if self.has_workspace_env_source() {
            self.env_source
        } else {
            // With no Workspace tab open, two of the three source modes would
            // be guaranteed-empty. Treat the hidden control as "Both" so a
            // persisted Workspace-only choice does not make globals vanish.
            crate::env_panel::EnvSource::Both
        }
    }

    /// The Environments panel row the selection is on, if any.
    pub(crate) fn selected_env_row(&self) -> Option<crate::env_panel::EnvRow> {
        let rows = self.env_rows();
        rows.get(self.global_env_idx.min(rows.len().saturating_sub(1)))
            .cloned()
    }

    /// The loaded environment the panel selection points at. `None` when the
    /// selected row is a workspace file that hasn't been opened yet (there is
    /// no environment to act on until it is).
    pub(crate) fn selected_env_id(&self) -> Option<u64> {
        self.selected_env_row().and_then(|r| r.env_id())
    }

    /// The index into `global_envs` of the panel's selected environment, for
    /// the operations that still address environments positionally (delete and
    /// its undo stack, activate).
    pub(crate) fn selected_env_index(&self) -> Option<usize> {
        let id = self.selected_env_id()?;
        self.global_envs.iter().position(|e| e.id == id)
    }

    /// Move the Environments panel selection onto whichever row holds `id`, so
    /// an environment that was just loaded, renamed or restored stays under the
    /// cursor even though the row order is the panel's, not `global_envs`'.
    pub(crate) fn select_env_row_by_id(&mut self, id: u64) {
        if let Some(i) = self.env_rows().iter().position(|r| r.env_id() == Some(id)) {
            self.global_env_idx = i;
            self.global_env_hscroll = 0;
        }
    }

    /// The Global Environment id that "Save Environment" / secret-reload
    /// actions currently target: the selected row in the Global Environments
    /// list, or (if open) the environment shown in the entries popup.
    pub(crate) fn current_env_id(&self) -> Option<u64> {
        if let Some(Overlay::EnvPopup(p)) = &self.overlay {
            return Some(p.env_id);
        }
        self.selected_env_id()
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
        // The selection indexes panel rows, and deleting an environment
        // removes one — clamp so it can't be left past the end.
        self.global_env_idx = self
            .global_env_idx
            .min(self.env_rows().len().saturating_sub(1));
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
        let id = env.id;
        self.global_envs.insert(idx, env);
        // Follow the environment to whichever panel row it landed on: the
        // panel's order isn't `global_envs`' once a workspace is open.
        self.select_env_row_by_id(id);
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.status = Some(crate::i18n::Status::EnvReopened(name));
        self.save_state();
    }

    /// Every theme offered in the Theme editor, in display order: the built-in
    /// presets first, then the user's own custom themes.
    pub(crate) fn all_themes(&self) -> Vec<crate::tui::theme::ThemeSpec> {
        crate::session::all_themes(&self.custom_themes)
    }

    /// The theme spec currently in effect: the manually-chosen theme if set
    /// (and still present), otherwise the current language's preset.
    pub(crate) fn active_theme_spec(&self) -> crate::tui::theme::ThemeSpec {
        crate::session::active_theme_spec(
            self.active_theme.as_deref(),
            &self.custom_themes,
            &self.language,
        )
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
        crate::session::effective_env(&self.collections, &self.global_envs, ci, self.active_env_id)
    }

    /// Keys defined in *both* the active collection's linked Environment and
    /// the active Global Environment — per `effective_env`'s merge rule the
    /// linked value always wins, so these keys' Global Environment value is
    /// silently shadowed. Used to flag such substitutions in the Request
    /// viewer with a warning icon so the collision isn't invisible.
    pub(crate) fn shadowed_env_keys(&self, ci: usize) -> std::collections::HashSet<String> {
        crate::session::shadowed_env_keys(
            &self.collections,
            &self.global_envs,
            ci,
            self.active_env_id,
        )
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

    /// Open the "import a whole Postman workspace" wizard.
    pub(crate) fn open_postman_wizard(&mut self) {
        let recent = self.session.recent_key_refs.clone();
        self.overlay = Some(Overlay::PostmanImport(Box::new(PostmanWizard::new(recent))));
    }

    /// Handle a key while the Postman import wizard is open.
    ///
    /// Everything that decides what happens next lives in
    /// [`crate::postman_flow`]; this maps keys onto it and drives the editors.
    pub(crate) fn on_key_postman(&mut self, mut w: Box<PostmanWizard>, key: KeyEvent) {
        let s = Strings::for_language(&self.language);
        let import_base = self
            .picker_dir(crate::session::PickerKind::Import)
            .map(std::path::Path::to_owned);
        w.remember_step();
        match w.stage() {
            PostmanStage::Connect => match key.code {
                // While the recent-keys dropdown has focus, Esc backs out of it
                // rather than leaving the wizard's connect step.
                KeyCode::Esc if w.recent_sel.is_some() => w.recent_sel = None,
                KeyCode::Esc => return,
                // On the key field, Down opens (or moves down in) the list of
                // references this key has been read from before, instead of
                // jumping to the workspace field.
                KeyCode::Down if w.field == 1 && !w.recent_entries().is_empty() => {
                    let last = w.recent_entries().len() - 1;
                    w.recent_sel = Some(w.recent_sel.map_or(0, |i| (i + 1).min(last)));
                }
                KeyCode::Up if w.recent_sel.is_some() => {
                    let i = w.recent_sel.unwrap();
                    w.recent_sel = if i == 0 { None } else { Some(i - 1) };
                }
                KeyCode::Tab | KeyCode::Down => {
                    w.field = (w.field + 1) % POSTMAN_CONNECT_FIELDS;
                    w.recent_sel = None;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    w.field = (w.field + POSTMAN_CONNECT_FIELDS - 1) % POSTMAN_CONNECT_FIELDS;
                    w.recent_sel = None;
                }
                // The key-source row is a choice, so Left/Right change it —
                // the same gesture that changes the import format two steps
                // later. On a text field they stay the editor's own.
                KeyCode::Left if w.field == 0 => {
                    w.key_source = w.key_source.cycled(false);
                    // The remembered entries belong to a source; the old
                    // selection would index into a different list.
                    w.recent_sel = None;
                }
                KeyCode::Right if w.field == 0 => {
                    w.key_source = w.key_source.cycled(true);
                    w.recent_sel = None;
                }
                KeyCode::Enter => {
                    // Picking a remembered reference fills the field and
                    // connects, rather than making the user press Enter twice.
                    if let Some(entry) = w
                        .recent_sel
                        .and_then(|i| w.recent_entries().get(i).cloned())
                    {
                        w.key = Editor::new(&entry, false);
                    }
                    w.recent_sel = None;
                    w.sync_fields();
                    w.flow.submit_connect(&s);
                    // A typed workspace id skips the listing and lands straight
                    // on the options, which want a suggested destination.
                    if matches!(w.flow.step(), Step::Options) {
                        w.suggest_dest(import_base.as_deref());
                    }
                }
                // The source row is a choice, not an editor: there is nothing
                // for a keystroke to type into.
                _ if w.field > 0 => {
                    // Typing anything else closes the dropdown and edits the
                    // field normally.
                    w.recent_sel = None;
                    let ed = match w.field {
                        1 => &mut w.key,
                        2 => &mut w.workspace_ref,
                        _ => &mut w.base_url,
                    };
                    apply_edit_key(ed, key);
                }
                _ => {}
            },
            // Esc during a background call cancels the whole wizard: there is
            // nothing partial to keep, and the worker cleans up after itself.
            PostmanStage::Loading => {
                if key.code == KeyCode::Esc {
                    w.flow.cancel();
                    return;
                }
            }
            PostmanStage::PickWorkspace => {
                let n = w.flow.visible_workspaces().len();
                match key.code {
                    KeyCode::Esc => {
                        w.flow.back_to_connect();
                    }
                    KeyCode::Up => w.flow.selected = w.flow.selected.saturating_sub(1),
                    KeyCode::Down if w.flow.selected + 1 < n => w.flow.selected += 1,
                    KeyCode::Enter => {
                        if w.flow.submit_workspace() {
                            w.suggest_dest(import_base.as_deref());
                        }
                    }
                    KeyCode::Backspace => {
                        w.flow.filter.pop();
                        w.flow.selected = 0;
                    }
                    KeyCode::Char(c) => {
                        w.flow.filter.push(c);
                        w.flow.selected = 0;
                    }
                    _ => {}
                }
            }
            PostmanStage::Options => {
                let on_dest = w.option_row == 0;
                match key.code {
                    KeyCode::Esc => {
                        // Back to wherever the workspace came from: the list if
                        // there is one, otherwise the key prompt.
                        if w.flow.workspaces().is_empty() {
                            w.flow.back_to_connect();
                        } else {
                            w.flow.to_pick_workspace();
                        }
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        w.option_row = (w.option_row + 1) % OPTION_ROWS;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        w.option_row = (w.option_row + OPTION_ROWS - 1) % OPTION_ROWS;
                    }
                    // Space toggles the row under the cursor, but only where a
                    // row *is* a toggle — on the path field it must still type.
                    KeyCode::Char(' ') if !on_dest => toggle_option_row(&mut w),
                    // Left/Right change the value on the row, as they do
                    // everywhere else in the app that offers a choice between
                    // two settings — the format row in particular reads as a
                    // pair of options laid out side by side, so pointing at
                    // one of them has to select it.
                    KeyCode::Left | KeyCode::Right if !on_dest => toggle_option_row(&mut w),
                    KeyCode::Enter => {
                        if w.option_row == OPTION_ROWS - 1 {
                            w.sync_fields();
                            w.flow.submit_options(&s);
                        } else if !on_dest {
                            toggle_option_row(&mut w);
                        } else {
                            // The destination is chosen in the file browser,
                            // like every other "save into a folder" in the app;
                            // typing a path by hand is not offered here because
                            // it was the one place that asked the user to know
                            // a path before seeing one.
                            self.open_postman_dest_browser(w);
                            return;
                        }
                    }
                    _ => {}
                }
            }
            PostmanStage::Confirm => match key.code {
                KeyCode::Esc => {
                    w.flow.cancel();
                    return;
                }
                KeyCode::Enter => {
                    w.flow.confirm();
                }
                _ => {}
            },
            PostmanStage::Downloading => {
                if key.code == KeyCode::Esc {
                    w.flow.cancel();
                    return;
                }
            }
            // The folder was already opened as a workspace by
            // `apply_postman_event`; this screen is just the receipt.
            PostmanStage::Done => return,
            PostmanStage::Error => {
                let back = w.before_error.clone();
                w.flow.clear_error(back);
            }
        }
        self.overlay = Some(Overlay::PostmanImport(w));
    }

    /// Park the wizard and open the file browser on its destination folder.
    /// The browser needs the overlay slot, so the wizard is stashed in
    /// `parked_postman` and restored by `finish_postman_dest` (or by the
    /// picker's Esc path) with everything else it holds intact.
    fn open_postman_dest_browser(&mut self, w: Box<PostmanWizard>) {
        self.postman_dest_seed_dir = w.dest_parent();
        self.parked_postman = Some(w);
        self.open_browser(FileAction::PostmanDestChooseFolder);
    }

    /// Take the folder chosen in the browser as the import destination and put
    /// the wizard back on screen.
    pub(crate) fn finish_postman_dest(&mut self, dir: std::path::PathBuf, name: String) {
        let Some(mut w) = self.parked_postman.take() else {
            return;
        };
        self.remember_picker_dir(crate::session::PickerKind::Import, &dir);
        self.save_state();
        // The browser refuses to commit a blank name, so this always names a
        // subfolder of the chosen parent — the import creates it, exactly as
        // the workspace save does.
        w.set_dest(dir.join(name.trim()));
        self.overlay = Some(Overlay::PostmanImport(w));
    }

    /// Poll the Postman wizard's worker (called each frame).
    pub(crate) fn poll_postman_updates(&mut self) {
        let Some(mut w) = take_overlay!(self, Overlay::PostmanImport(w) => w) else {
            return;
        };
        let s = Strings::for_language(&self.language);
        // Tracked every tick, not just on a keypress: a download runs with
        // nobody touching the keyboard, so a failure during it used to be
        // blamed on the *confirmation* — the last screen a key was pressed on —
        // and dismissing it offered "start the import" again, straight back
        // into the same wall. The step being interrupted is the one on screen.
        w.remember_step();
        let event = w.flow.poll(&s);
        // Once the key has actually worked, keep the reference: finding the
        // 1Password item path is the tedious half of setting an import up.
        let learned = w
            .flow
            .key_to_remember()
            .map(str::to_string)
            .is_some_and(|key| self.session.remember_key_ref(&key));
        if learned {
            self.save_state();
        }
        self.overlay = Some(Overlay::PostmanImport(w));
        if let Some(PostmanEvent::Imported(summary)) = event {
            self.apply_postman_event(*summary);
        }
    }

    /// A finished import: open the folder it produced as a Workspace, exactly
    /// as if the user had browsed to it — the point of the whole feature.
    pub(crate) fn apply_postman_event(&mut self, summary: ImportSummary) {
        // Only one status line fits, so the more actionable of the two wins:
        // missing data beats a note about data that was deliberately dropped.
        if !summary.failures.is_empty() {
            self.status = Some(Status::PostmanSkipped(summary.failures.len()));
        } else if summary.converted_with_notes {
            self.status = Some(Status::PostmanNotes);
        }
        self.confirm_workspace_root(summary.dest);
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
        // Refuse to write a file PaperBoy couldn't read back (see
        // `SaveCollection`): an empty-path file field breaks reparsing.
        if let Some((req, field)) = self.collections[ci].first_empty_file_field() {
            self.status = Some(Status::SaveUnreadableEmptyFile { req, field });
            return false;
        }
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

    /// Close the wizard. Any temp repo it fetched is owned by the flow, which
    /// cleans it up as it is dropped.
    pub(crate) fn close_remote(&mut self, _w: Box<RemoteWizard>) {
        self.overlay = None;
    }

    /// Handle a key while the remote-git wizard is open.
    pub(crate) fn on_key_remote(&mut self, mut w: Box<RemoteWizard>, key: KeyEvent) {
        match w.stage() {
            RemoteStage::Connect => match key.code {
                // While the recent-URLs dropdown has focus, Esc backs out of it
                // rather than closing the whole wizard.
                KeyCode::Esc if w.recent_sel.is_some() => w.recent_sel = None,
                KeyCode::Esc => return self.close_remote(w),
                KeyCode::Tab | KeyCode::BackTab => {
                    w.field = 1 - w.field;
                    w.recent_sel = None;
                }
                // On the URL field, Down opens (or moves down in) the recent-URLs
                // dropdown instead of jumping to the token field.
                KeyCode::Down if w.field == 0 && !w.recent.is_empty() => {
                    let last = w.recent.len() - 1;
                    w.recent_sel = Some(w.recent_sel.map_or(0, |i| (i + 1).min(last)));
                }
                KeyCode::Up if w.recent_sel.is_some() => {
                    let i = w.recent_sel.unwrap();
                    w.recent_sel = if i == 0 { None } else { Some(i - 1) };
                }
                KeyCode::Up | KeyCode::Down => {
                    w.field = 1 - w.field;
                    w.recent_sel = None;
                }
                KeyCode::Enter => {
                    // Picking a recent URL connects immediately, rather than
                    // just populating the field (which would force the user to
                    // press Enter a second time).
                    if let Some(url) = w.recent_sel.and_then(|i| w.recent.get(i)).cloned() {
                        w.url = Editor::new(&url, false);
                    }
                    w.recent_sel = None;
                    w.sync_fields();
                    if w.flow.url.trim().is_empty() {
                        let s = Strings::for_language(&self.language);
                        w.flow.fail(s.git_url_required.to_string());
                    } else {
                        w.reset_list();
                        w.flow.connect();
                    }
                }
                _ => {
                    // Typing anything else closes the dropdown and edits the
                    // field normally.
                    w.recent_sel = None;
                    let ed = if w.field == 0 {
                        &mut w.url
                    } else {
                        &mut w.token
                    };
                    apply_edit_key(ed, key);
                }
            },
            RemoteStage::Loading => {
                if key.code == KeyCode::Esc {
                    return self.close_remote(w);
                }
            }
            RemoteStage::PickRef => {
                let s = Strings::for_language(&self.language);
                let choices = w.flow.ref_choices(&s);
                let vis = filter_indices(choices.iter().map(|r| r.label.as_str()), &w.filter);
                match key.code {
                    KeyCode::Esc => return self.close_remote(w),
                    KeyCode::Up => w.sel = w.sel.saturating_sub(1),
                    KeyCode::Down if w.sel + 1 < vis.len() => w.sel += 1,
                    KeyCode::Enter => {
                        if let Some(&ri) = vis.get(w.sel) {
                            let choice = choices[ri].clone();
                            w.reset_list();
                            w.flow.choose_ref(choice);
                        }
                    }
                    KeyCode::Backspace => {
                        w.filter.pop();
                        w.sel = 0;
                    }
                    KeyCode::Char(c) => {
                        w.filter.push(c);
                        w.sel = 0;
                    }
                    _ => {}
                }
            }
            RemoteStage::PickFile => {
                let files = w.flow.pickable_files();
                let vis = filter_indices(files.iter().map(|s| s.as_str()), &w.filter);
                match key.code {
                    KeyCode::Esc => return self.close_remote(w),
                    KeyCode::Up => w.sel = w.sel.saturating_sub(1),
                    KeyCode::Down if w.sel + 1 < vis.len() => w.sel += 1,
                    KeyCode::Enter => {
                        if let Some(&fi) = vis.get(w.sel) {
                            let path = files[fi].clone();
                            w.reset_list();
                            w.flow.choose_file(path);
                        }
                    }
                    KeyCode::Backspace => {
                        w.filter.pop();
                        w.sel = 0;
                    }
                    KeyCode::Char(c) => {
                        w.filter.push(c);
                        w.sel = 0;
                    }
                    _ => {}
                }
            }
            RemoteStage::PickWorkspaceFilter => match key.code {
                KeyCode::Esc => return self.close_remote(w),
                KeyCode::Up => w.sel = w.sel.saturating_sub(1),
                KeyCode::Down => w.sel = (w.sel + 1).min(WorkspaceGitFilter::ALL.len() - 1),
                KeyCode::Enter => {
                    let choice = WorkspaceGitFilter::ALL[w.sel];
                    if !w.flow.all_files().iter().any(|p| choice.matches(p)) {
                        let s = Strings::for_language(&self.language);
                        w.flow.fail(s.git_workspace_no_matches.to_string());
                    } else {
                        w.flow.choose_workspace_filter(choice);
                    }
                }
                _ => {}
            },
            // Any key dismisses the error, which returns to the step it came
            // from rather than throwing away everything fetched so far.
            RemoteStage::Error => {
                w.flow.clear_error();
                w.reset_list();
            }
        }
        self.overlay = Some(Overlay::RemoteGit(w));
    }

    /// Poll the wizard's in-flight git operation (called each frame).
    pub(crate) fn poll_git_updates(&mut self) {
        let Some(mut w) = take_overlay!(self, Overlay::RemoteGit(w) => w) else {
            return;
        };
        match w.flow.poll() {
            Some(event) => {
                let keep_open = self.apply_flow_event(&w, event);
                if keep_open {
                    self.overlay = Some(Overlay::RemoteGit(w));
                }
            }
            None => self.overlay = Some(Overlay::RemoteGit(w)),
        }
    }

    /// Act on a completed load. Returns whether the wizard should stay open
    /// (false = something was loaded, so close it).
    pub(crate) fn apply_flow_event(&mut self, w: &RemoteWizard, event: FlowEvent) -> bool {
        match event {
            FlowEvent::Workspace { root, name, origin } => {
                self.remember_git_url(&w.flow.url);
                // Ask the user whether to keep this download temporary (the old
                // default behaviour) or save it to a permanent, chosen location
                // right away — see `Overlay::WorkspaceStorageChoice`.
                self.overlay = Some(Overlay::WorkspaceStorageChoice {
                    repo: root,
                    name,
                    origin,
                    sel: 0,
                });
                false
            }
            FlowEvent::Content { path, text, origin } => {
                self.remember_git_url(&w.flow.url);
                match w.kind() {
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
                    // A Workspace load never produces `Content` — it takes the
                    // filter/download path instead. Unreachable in practice.
                    RemoteKind::Workspace => false,
                }
            }
        }
    }

    /// Close the "save to git" wizard. Unlike the load wizard there is no
    /// temp repo to clean up here — the background push manages (and always
    /// cleans up) its own throwaway repo internally, in one shot; the wizard
    /// itself is simply dropped by the caller.
    pub(crate) fn close_git_save(&mut self) {
        self.overlay = None;
    }

    /// Handle a key while the "save to git" wizard is open.
    ///
    /// Every decision about what a keystroke *means* for the save itself lives
    /// in [`crate::save_flow`]; this maps keys onto it and manages the editors.
    pub(crate) fn on_key_git_save(&mut self, mut w: Box<GitSaveWizard>, key: KeyEvent) {
        let s = Strings::for_language(&self.language);
        match w.stage() {
            GitSaveStage::Connect => match key.code {
                KeyCode::Esc => return self.close_git_save(),
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    w.field = 1 - w.field;
                }
                KeyCode::Enter => {
                    w.sync();
                    w.flow.submit_connect(&s);
                    w.field = 0;
                }
                _ => {
                    let ed = if w.field == 0 {
                        &mut w.url
                    } else {
                        &mut w.token
                    };
                    apply_edit_key(ed, key);
                }
            },
            GitSaveStage::ChoosePaths => {
                // Only the fields actually on screen take focus: the checkbox
                // and the env path are absent without an environment, and the
                // env path is hidden while the checkbox is unticked.
                let mut visible = vec![0u8];
                if w.has_env() {
                    visible.push(1);
                    if w.flow.include_env {
                        visible.push(2);
                    }
                }
                match key.code {
                    KeyCode::Esc => return self.close_git_save(),
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                        let idx = visible.iter().position(|f| *f == w.field).unwrap_or(0);
                        let back = matches!(key.code, KeyCode::BackTab | KeyCode::Up);
                        let n = visible.len();
                        let next = if back {
                            (idx + n - 1) % n
                        } else {
                            (idx + 1) % n
                        };
                        w.field = visible[next];
                    }
                    KeyCode::Char(' ') if w.field == 1 => {
                        w.flow.include_env = !w.flow.include_env;
                    }
                    KeyCode::Enter => {
                        w.sync();
                        if w.flow.submit_paths(&s) {
                            w.sel = None;
                        }
                    }
                    _ => {
                        let ed = match w.field {
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
            GitSaveStage::ChooseTarget => {
                let branches = w.flow.refs().branches.clone();
                match key.code {
                    KeyCode::Esc if w.sel.is_some() => w.sel = None,
                    KeyCode::Esc => return self.close_git_save(),
                    KeyCode::Tab | KeyCode::BackTab => {
                        w.flow.target_kind = if w.flow.target_kind == SaveTargetKind::Branch {
                            SaveTargetKind::Tag
                        } else {
                            SaveTargetKind::Branch
                        };
                    }
                    KeyCode::Down if w.sel.is_none() => {
                        if !branches.is_empty() {
                            w.sel = Some(0);
                        }
                    }
                    KeyCode::Down => {
                        if let Some(i) = w.sel {
                            w.sel = Some((i + 1).min(branches.len().saturating_sub(1)));
                        }
                    }
                    KeyCode::Up => {
                        if let Some(i) = w.sel {
                            w.sel = if i == 0 { None } else { Some(i - 1) };
                        }
                    }
                    KeyCode::Enter if w.sel.is_some() => {
                        if let Some(name) = w.sel.and_then(|i| branches.get(i)) {
                            w.target_name = Editor::new(name, false);
                            w.flow.target_kind = SaveTargetKind::Branch;
                        }
                        w.sel = None;
                    }
                    KeyCode::Enter => {
                        w.sync();
                        if w.flow.submit_target() {
                            // The flow may have restored a cleared commit
                            // message; show what it will actually push.
                            w.commit_msg = Editor::new(&w.flow.message, false);
                        }
                    }
                    KeyCode::Backspace if w.sel.is_none() => {
                        w.target_name.backspace();
                    }
                    KeyCode::Char(c) if w.sel.is_none() => {
                        w.target_name.insert(c);
                    }
                    _ => {
                        // Typing anything while the dropdown is open closes it
                        // and edits the field normally (matches the load
                        // wizard's recent-URL dropdown behaviour).
                        w.sel = None;
                        apply_edit_key(&mut w.target_name, key);
                    }
                }
            }
            GitSaveStage::CommitMessage => match key.code {
                KeyCode::Esc => return self.close_git_save(),
                KeyCode::Enter => {
                    w.sync();
                    let payload = self.git_save_payload(&w, &s);
                    w.flow.submit_message(payload);
                }
                _ => apply_edit_key(&mut w.commit_msg, key),
            },
            GitSaveStage::Pushing => {
                if key.code == KeyCode::Esc {
                    return self.close_git_save();
                }
            }
            GitSaveStage::Done | GitSaveStage::Error => return self.close_git_save(),
        }
        self.overlay = Some(Overlay::GitSave(w));
    }

    /// Assemble the files this save will commit, reading whatever the wizard is
    /// pointed at out of the app's own tabs. The assembly rules themselves
    /// (including the refusals) belong to [`crate::save_flow`].
    fn git_save_payload(
        &self,
        w: &GitSaveWizard,
        s: &Strings,
    ) -> Result<Vec<crate::save_flow::SaveFile>, String> {
        match &w.flow.source {
            SaveSource::Workspace { root, .. } => SaveFlow::workspace_payload(root, s),
            SaveSource::Collection { ci } => {
                let col = self
                    .collections
                    .get(*ci)
                    .ok_or_else(|| s.git_save_source_gone.to_string())?;
                w.flow.collection_payload(col, w.env.as_ref(), s)
            }
            SaveSource::Report { report_idx } => {
                let rt = self
                    .reports
                    .get(*report_idx)
                    .ok_or_else(|| s.git_save_source_gone.to_string())?;
                Ok(w.flow.report_payload(&rt.report))
            }
        }
    }

    /// Poll the "save to git" wizard's in-flight background op (called each
    /// frame).
    pub(crate) fn poll_git_save_updates(&mut self) {
        let Some(mut w) = take_overlay!(self, Overlay::GitSave(w) => w) else {
            return;
        };
        let s = Strings::for_language(&self.language);
        if let Some(crate::save_flow::SaveEvent::Pushed { commit_sha }) = w.flow.poll(&s) {
            self.finish_git_save(&w, &commit_sha);
        }
        self.overlay = Some(Overlay::GitSave(w));
    }

    /// After a successful push: clear the "new"/"modified" markers, exactly as
    /// a local save does, and — for a **branch** target only — remember where
    /// the item now lives. A tag save clears the markers too but leaves the
    /// remembered branch origin alone: a tag is a snapshot, so later edits must
    /// keep following the branch rather than a frozen point on it.
    fn finish_git_save(&mut self, w: &GitSaveWizard, new_sha: &str) {
        let repin = w.flow.target_kind.repins_origin();
        match &w.flow.source {
            SaveSource::Report { report_idx } => {
                // A report has no per-request markers; just clear its dirty
                // flag and repin it like a collection.
                if let Some(rt) = self.reports.get_mut(*report_idx) {
                    rt.report.dirty = false;
                    if repin {
                        rt.report.git_origin = Some(w.flow.pushed_origin());
                    }
                }
            }
            SaveSource::Workspace { ci, filter, .. } => {
                // A workspace push commits the on-disk tree, not the in-memory
                // collection, so there are no per-request markers to clear.
                // Repin to the exact commit just pushed, so a later redownload
                // fetches this state rather than whatever the branch points at
                // by then.
                if repin && let Some(col) = self.collections.get_mut(*ci) {
                    col.workspace_git_origin = Some(WorkspaceGitOrigin {
                        repo_url: w.flow.url.trim().to_string(),
                        commit_sha: new_sha.to_string(),
                        ref_kind: RefKind::Branch,
                        ref_name: w.flow.target_name.trim().to_string(),
                        filter: *filter,
                    });
                }
            }
            SaveSource::Collection { ci } => {
                let ci = *ci;
                self.mark_collection_saved(ci);
                let env_id = w
                    .flow
                    .include_env
                    .then(|| w.env.as_ref().map(|e| e.id))
                    .flatten();
                if let Some(env_id) = env_id {
                    self.mark_env_saved(env_id);
                }
                if repin {
                    let origin = w.flow.pushed_origin();
                    if let Some(col) = self.collections.get_mut(ci) {
                        col.git_origin = Some(origin.clone());
                    }
                    // The environment went up in the same commit, so it is
                    // reachable at its own path on the same branch.
                    if let Some(env_id) = env_id
                        && let Some(env) = self.global_envs.iter_mut().find(|e| e.id == env_id)
                    {
                        env.git_origin = Some(GitOrigin {
                            path: w.flow.env_path.trim().to_string(),
                            ..origin
                        });
                    }
                }
            }
        }
        self.remember_git_url(&w.flow.url);
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

/// Flip the toggle on the Options row under the cursor. Row 0 is the
/// destination path, which is a text field rather than a toggle.
fn toggle_option_row(w: &mut PostmanWizard) {
    match w.option_row {
        1 => w.flow.include_collections = !w.flow.include_collections,
        2 => w.flow.include_environments = !w.flow.include_environments,
        3 => {
            w.flow.format = match w.flow.format {
                ImportFormat::Raw => ImportFormat::Hurl,
                ImportFormat::Hurl => ImportFormat::Raw,
            }
        }
        4 => w.flow.overwrite = !w.flow.overwrite,
        _ => {}
    }
}
