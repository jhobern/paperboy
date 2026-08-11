//! Git remote load/save for the GUI — load collections/environments from, and
//! save them to, a git remote **with no local clone** (only the files being
//! read/written are fetched), driven by the shared [`crate::git_remote`]
//! module (the same core the terminal UI uses).
//!
//! A whole **Workspace** can be loaded this way too: instead of picking one
//! file, you pick a file-*type* filter and every matching file in the chosen
//! ref is checked out into a throwaway folder that becomes the new tab's
//! workspace root (see [`LoadTarget::Workspace`]).
//!
//! All git state lives here so the rest of the GUI only needs a single
//! [`RemoteUi`] field on [`GuiApp`] plus one [`show`] call per frame; menu
//! entries kick a flow off with [`RemoteUi::open_load`] /
//! [`RemoteUi::open_load_workspace`] / [`RemoteUi::open_save_collection`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;

use crate::git_remote::{self, GitOrigin, RefKind};
use crate::i18n::{Status, Strings};
use crate::remote_flow::{
    FlowEvent, RefChoice, RemoteFlow, RemoteKind, Step, WorkspaceGitFilter, WorkspaceGitOrigin,
};

use crate::environment::Environment;
use crate::save_flow::{
    SaveEvent, SaveFlow as CoreSaveFlow, SaveSource, SaveTargetKind, Step as CoreStep,
};

use super::app::GuiApp;

const REPAINT_WHILE_BUSY: Duration = Duration::from_millis(100);

/// All Git-remote UI state (wizard step, fetched refs/file lists, in-flight
/// background work). Owned by [`GuiApp`]; `Default` = nothing in progress.
#[derive(Default)]
pub struct RemoteUi {
    /// The active flow, or `None` when no Git dialog is open.
    flow: Option<Flow>,
}

enum Flow {
    Load(LoadFlow),
    Save(SaveFlow),
}

/// What the save dialog was opened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveTarget {
    Collection(usize),
    /// A whole workspace tab previously downloaded from git.
    Workspace(usize),
    /// The open report editor. The GUI has only ever one report open at a time,
    /// so unlike the terminal UI there is no index to carry.
    Report,
}

/// What a load flow is fetching: one file (a collection or an environment,
/// told apart by its extension once picked), or every file matching a
/// [`WorkspaceGitFilter`] as a whole Workspace tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadTarget {
    File,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadStep {
    Connect,
    PickRef,
    PickFile,
    /// Workspace load only: choose which file types to actually download,
    /// before anything is checked out. A repo may hold plenty of large,
    /// unrelated files that have no business being pulled down just to browse
    /// its collections, so nothing is fetched until this is answered.
    PickWorkspaceFilter,
    /// Workspace load only: the files are downloaded and sitting in a temp
    /// folder — keep it there, or copy it somewhere permanent right now?
    WorkspaceStorage,
}

struct LoadFlow {
    /// The shared state machine: which step, what is fetched, what is kept.
    /// See [`crate::remote_flow`] — the terminal UI drives the same one, so
    /// the two front-ends cannot disagree about how a load behaves.
    flow: RemoteFlow,
    target: LoadTarget,
    /// Which of the two ref lists is showing. The GUI presents branches and
    /// tags separately, where the terminal UI offers one filterable list.
    ref_kind: RefKind,
    selected_branch: usize,
    selected_tag: usize,
    filter: String,
    show_all_files: bool,
    selected_path: Option<String>,
    /// Workspace load only: which file types to download.
    ws_filter: WorkspaceGitFilter,
    /// Workspace load only: the downloaded folder, held between the
    /// `WorkspaceStorage` question and the answer that consumes it.
    ws_root: Option<PathBuf>,
    /// Workspace load only: the name proposed for the tab (and for a
    /// permanent folder), derived from the repo URL.
    ws_name: String,
    /// Workspace load only: the provenance the flow worked out, held alongside
    /// `ws_root` until the tab is created.
    ws_origin: Option<WorkspaceGitOrigin>,
    /// An error raised by the GUI itself (a blank URL, nothing picked yet).
    /// Errors from git live on the flow; [`LoadFlow::error`] shows either.
    local_error: Option<String>,
    /// The last answer [`LoadFlow::visible_files`] gave, with what it was an
    /// answer to. The picker asks for the list on every frame it is on screen,
    /// and a repo with a few thousand files made that a full lowercase-and-
    /// substring pass plus a clone of every matching path per frame — for a
    /// list that only changes when the user types in the filter box.
    ///
    /// Handed out as an `Rc` so serving it costs a refcount rather than the
    /// clone the cache was added to avoid.
    visible_cache: std::cell::RefCell<Option<VisibleFiles>>,
}

/// A memoised [`LoadFlow::visible_files`] result, with the inputs it was
/// computed from. The listing itself is identified by
/// [`RemoteFlow::files_generation`] rather than by anything derived from its
/// contents — two different repos can easily agree on a file count, and on a
/// missing commit sha.
struct VisibleFiles {
    generation: u64,
    filter: String,
    show_all: bool,
    out: std::rc::Rc<Vec<String>>,
}

impl LoadFlow {
    fn new(target: LoadTarget) -> Self {
        Self {
            // A "file" load doesn't ask up front whether it is fetching a
            // collection or an environment — it decides from the content once
            // fetched — so it borrows Collection's step sequence and does its
            // own file narrowing below.
            flow: RemoteFlow::new(match target {
                LoadTarget::File => RemoteKind::Collection,
                LoadTarget::Workspace => RemoteKind::Workspace,
            }),
            target,
            ref_kind: RefKind::Branch,
            selected_branch: 0,
            selected_tag: 0,
            filter: String::new(),
            show_all_files: false,
            selected_path: None,
            ws_filter: WorkspaceGitFilter::HurlAndJson,
            ws_root: None,
            ws_name: String::new(),
            ws_origin: None,
            local_error: None,
            visible_cache: std::cell::RefCell::new(None),
        }
    }

    fn step(&self) -> LoadStep {
        // Once the download has landed the flow is done, but the GUI still has
        // its "keep or save?" question to ask.
        if self.ws_root.is_some() {
            return LoadStep::WorkspaceStorage;
        }
        match self.flow.step() {
            Step::Connect => LoadStep::Connect,
            Step::PickRef => LoadStep::PickRef,
            Step::PickFile => LoadStep::PickFile,
            Step::PickWorkspaceFilter => LoadStep::PickWorkspaceFilter,
            Step::WorkspaceStorage => LoadStep::WorkspaceStorage,
        }
    }

    fn is_busy(&self) -> bool {
        self.flow.is_busy()
    }

    fn busy_label(&self, s: &Strings) -> Option<&'static str> {
        self.flow.busy().map(|phase| phase.label(s))
    }

    fn error(&self) -> Option<&str> {
        self.local_error.as_deref().or_else(|| self.flow.error())
    }

    fn clear_errors(&mut self) {
        self.local_error = None;
        self.flow.clear_error();
    }

    fn selected_ref_name(&self) -> Option<&str> {
        let refs = self.flow.refs();
        match self.ref_kind {
            RefKind::Branch => refs.branches.get(self.selected_branch),
            RefKind::Tag => refs.tags.get(self.selected_tag),
        }
        .map(String::as_str)
    }

    fn selected_gitref(&self) -> Option<String> {
        self.selected_ref_name().map(|name| match self.ref_kind {
            RefKind::Branch => git_remote::branch_ref(name),
            RefKind::Tag => git_remote::tag_ref(name),
        })
    }

    /// The files to offer. A "file" load can end up being either a collection
    /// or an environment, so unlike the terminal UI — which asks which up
    /// front and narrows to that one kind — this shows both, with a "show all"
    /// escape hatch for a repo that names its files unusually.
    fn visible_files(&self) -> std::rc::Rc<Vec<String>> {
        let generation = self.flow.files_generation();
        if let Some(hit) = self.visible_cache.borrow().as_ref()
            && hit.generation == generation
            && hit.filter == self.filter
            && hit.show_all == self.show_all_files
        {
            return hit.out.clone();
        }
        let filter = self.filter.to_lowercase();
        let out: std::rc::Rc<Vec<String>> = std::rc::Rc::new(
            self.flow
                .all_files()
                .iter()
                .filter(|path| self.show_all_files || is_default_load_file(path))
                .filter(|path| filter.is_empty() || path.to_lowercase().contains(&filter))
                .cloned()
                .collect(),
        );
        *self.visible_cache.borrow_mut() = Some(VisibleFiles {
            generation,
            filter: self.filter.clone(),
            show_all: self.show_all_files,
            out: out.clone(),
        });
        out
    }

    /// How many repo files the current [`WorkspaceGitFilter`] would download.
    ///
    /// A count, not a list: the filter step draws this number on every frame
    /// and wants nothing else from it, and the download itself re-applies the
    /// filter on the far side of the flow rather than being handed a list.
    fn workspace_match_count(&self) -> usize {
        self.flow
            .all_files()
            .iter()
            .filter(|p| self.ws_filter.matches(p))
            .count()
    }

    /// Advance the shared flow, and act on anything it finishes. Returns true
    /// when the dialog should close because something was loaded.
    fn poll(&mut self, app: &mut GuiApp) -> bool {
        match self.flow.poll() {
            Some(FlowEvent::Content { text, origin, .. }) => {
                self.finish_loaded_content(app, text, origin)
            }
            Some(FlowEvent::Workspace { root, name, origin }) => {
                // Hold the download until the user answers "keep it here, or
                // save it somewhere permanent?" on the next step.
                self.ws_root = Some(root);
                self.ws_name = name;
                self.ws_origin = origin;
                false
            }
            None => false,
        }
    }

    /// Create the Workspace tab from the downloaded folder now sitting at
    /// `ws_root`, and close the wizard. Returns `false` (leaving the dialog
    /// open on its error) if the download was somehow lost.
    fn finish_workspace(&mut self, app: &mut GuiApp) -> bool {
        let Some(root) = self.ws_root.take() else {
            self.local_error = Some(app.strings.gui_git_err_browse_again.to_string());
            return false;
        };
        let name = nonblank(&self.ws_name).unwrap_or_else(|| file_stem_from_url(&self.flow.url));
        remember_git_url(&mut app.session, &self.flow.url);
        app.session
            .open_workspace_from_git(root, name, self.ws_origin.take());
        true
    }

    fn finish_loaded_content(
        &mut self,
        app: &mut GuiApp,
        content: String,
        origin: Option<GitOrigin>,
    ) -> bool {
        let Some(path) = self.selected_path.clone() else {
            self.local_error = Some(app.strings.gui_git_err_no_file.to_string());
            return false;
        };
        let Some(origin) = origin else {
            self.local_error = Some(app.strings.gui_git_err_no_ref.to_string());
            return false;
        };
        let name = name_from_repo_path(&path);

        remember_git_url(&mut app.session, &self.flow.url);
        // `.json` covers both a Postman collection and a Postman environment,
        // so the extension alone can't say which this is — ask the content.
        if is_vars_file(&path) || crate::postman::postman_env_values(&content).is_some() {
            if app
                .session
                .load_environment_text(name, &content, None, Some(origin))
                .is_some()
            {
                return true;
            }
            self.local_error = Some(app.strings.gui_git_err_not_env.to_string());
            return false;
        }

        if app.session.load_collection_text(name, &content, None) {
            if let Some(col) = app.session.collections.last_mut() {
                col.git_origin = Some(origin);
            }
            app.session.save();
            true
        } else {
            self.local_error = Some(app.strings.gui_git_err_not_collection.to_string());
            false
        }
    }
}

/// The GUI's presentation of [`crate::save_flow`].
///
/// Like [`LoadFlow`], everything about *what a save does* lives in the shared
/// flow — which the terminal UI drives too — and this owns only the dialog:
/// which step is on screen, the branch dropdown selection, and errors the GUI
/// raises itself before the flow is involved.
struct SaveFlow {
    target: SaveTarget,
    /// The shared state machine, built on the first frame once the app's tabs
    /// can be read.
    flow: Option<CoreSaveFlow>,
    /// A snapshot of the collection's effective environment, used to build the
    /// `.vars` that can ride along in the same commit.
    env: Option<Environment>,
    /// True when the target tab no longer exists, so saving is impossible; this
    /// disables the buttons without matching on error text.
    blocked: bool,
    /// The branch picked from the dropdown, if the user used it.
    selected_branch: usize,
    /// The last step that wasn't a failure. The shared flow replaces the whole
    /// step with `Failed`, which is right for the terminal UI's dedicated error
    /// screen but wrong here: the GUI shows the error inline above the step, so
    /// a rejected path must leave the user on the path step to fix it rather
    /// than throwing them back to the URL.
    last_step: SaveStep,
    /// An error the GUI raised itself, kept apart from the flow's own so
    /// neither can hide the other.
    local_error: Option<String>,
}

impl SaveFlow {
    fn new(target: SaveTarget) -> Self {
        Self {
            target,
            flow: None,
            env: None,
            blocked: false,
            selected_branch: 0,
            last_step: SaveStep::Connect,
            local_error: None,
        }
    }

    /// Build the shared flow from whatever the dialog was opened on. Deferred to
    /// the first frame because the tabs aren't reachable when the menu item is
    /// clicked.
    fn ensure_initialized(&mut self, app: &GuiApp) {
        if self.flow.is_some() || self.blocked {
            return;
        }
        match self.target {
            SaveTarget::Collection(ci) => {
                let Some(col) = app.session.collections.get(ci) else {
                    return self.block(app.strings.gui_git_err_collection_missing);
                };
                let env = app.session.effective_env(ci);
                self.flow = Some(CoreSaveFlow::for_collection(ci, col, env.as_ref()));
                self.env = env;
            }
            SaveTarget::Workspace(ci) => {
                let Some(col) = app.session.collections.get(ci) else {
                    return self.block(app.strings.gui_git_err_collection_missing);
                };
                let Some(origin) = col.workspace_git_origin.clone() else {
                    return self.block(app.strings.gui_git_err_ws_not_from_git);
                };
                self.flow = Some(CoreSaveFlow::for_workspace(ci, col, &origin));
            }
            SaveTarget::Report => {
                let Some(editor) = app.report_editor.as_ref() else {
                    return self.block(app.strings.gui_git_err_collection_missing);
                };
                self.flow = Some(CoreSaveFlow::for_report(0, &editor.report));
            }
        }
    }

    fn block(&mut self, message: &str) {
        self.blocked = true;
        self.local_error = Some(message.to_string());
    }

    fn is_busy(&self) -> bool {
        self.flow.as_ref().is_some_and(CoreSaveFlow::is_busy)
    }

    /// Which step to draw. A blocked dialog stays on the first step so its
    /// explanation is visible; a failure keeps the user wherever they were,
    /// with the reason shown above it.
    fn step(&self) -> SaveStep {
        let Some(flow) = self.flow.as_ref() else {
            return SaveStep::Connect;
        };
        match flow.step() {
            CoreStep::Failed(_) => self.last_step,
            CoreStep::Connect => SaveStep::Connect,
            CoreStep::ChoosePaths => SaveStep::ChoosePaths,
            CoreStep::ChooseTarget => SaveStep::ChooseTarget,
            CoreStep::CommitMessage => SaveStep::CommitMessage,
            CoreStep::Done => SaveStep::Done,
        }
    }

    /// Remember the step being drawn, so a later failure can return to it.
    /// Called once per frame, before drawing.
    fn remember_step(&mut self) {
        let step = self.step();
        self.last_step = step;
    }

    /// Whichever error is current: the GUI's own, or the flow's.
    fn error(&self) -> Option<&str> {
        self.local_error
            .as_deref()
            .or_else(|| self.flow.as_ref().and_then(CoreSaveFlow::error))
    }

    fn clear_errors(&mut self) {
        self.local_error = None;
        if let Some(flow) = self.flow.as_mut() {
            flow.clear_error();
        }
    }

    /// The label for the operation in flight, so the dialog says what it is
    /// waiting on rather than just greying out.
    fn busy_label(&self, s: &Strings) -> Option<&'static str> {
        self.flow.as_ref()?.busy().map(|p| p.label(s))
    }

    fn poll(&mut self, app: &mut GuiApp) -> bool {
        let Some(flow) = self.flow.as_mut() else {
            return false;
        };
        match flow.poll(&app.strings) {
            Some(SaveEvent::Pushed { commit_sha }) => self.finish_save(app, &commit_sha),
            None => false,
        }
    }

    /// After a successful push: clear the tab's edit markers exactly as a local
    /// save does and, for a branch target only, remember where it now lives. A
    /// tag is a snapshot, so later edits keep following the branch.
    fn finish_save(&mut self, app: &mut GuiApp, commit_sha: &str) -> bool {
        let Some(flow) = self.flow.as_ref() else {
            return false;
        };
        let repin = flow.target_kind.repins_origin();
        match flow.source {
            SaveSource::Collection { ci } => {
                let Some(col) = app.session.collections.get_mut(ci) else {
                    self.local_error = Some(app.strings.gui_git_err_collection_closed.to_string());
                    return false;
                };
                if repin {
                    col.git_origin = Some(flow.pushed_origin());
                }
                // The push *is* this collection's save, so clear its edit
                // markers exactly as a local Save does.
                col.mark_saved();
                if repin
                    && flow.include_env
                    && let Some(env_id) = self.env.as_ref().map(|e| e.id)
                    && let Some(env) = app.session.global_envs.iter_mut().find(|e| e.id == env_id)
                {
                    env.git_origin = Some(GitOrigin {
                        path: flow.env_path.trim().to_string(),
                        ..flow.pushed_origin()
                    });
                }
            }
            SaveSource::Workspace { ci, filter, .. } => {
                // A workspace push commits the folder as it sits on disk, so
                // there are no per-request markers to clear. Repin to the exact
                // commit just pushed, so a later redownload fetches this state
                // rather than wherever the branch has moved to by then.
                if repin && let Some(col) = app.session.collections.get_mut(ci) {
                    col.workspace_git_origin = Some(WorkspaceGitOrigin {
                        repo_url: flow.url.trim().to_string(),
                        commit_sha: commit_sha.to_string(),
                        ref_kind: RefKind::Branch,
                        ref_name: flow.target_name.trim().to_string(),
                        filter,
                    });
                }
            }
            SaveSource::Report { .. } => {
                if let Some(editor) = app.report_editor.as_mut() {
                    editor.report.dirty = false;
                    if repin {
                        editor.report.git_origin = Some(flow.pushed_origin());
                    }
                }
            }
        }

        remember_git_url(&mut app.session, &flow.url);
        app.session.status = Some(Status::GitSaved);
        app.session.save();
        true
    }
}

/// Which step of the save dialog is on screen. Derived from the shared flow by
/// [`SaveFlow::step`] rather than tracked alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveStep {
    Connect,
    ChoosePaths,
    ChooseTarget,
    CommitMessage,
    Done,
}

#[derive(Default)]
enum UiAction {
    #[default]
    None,
    Cancel,
    Connect,
    BackToConnect,
    BrowseFiles,
    BackToRefs,
    LoadFile,
    /// Download the Workspace files matching the chosen filter.
    DownloadWorkspace,
    /// Keep the downloaded Workspace in its temp folder and open it.
    KeepWorkspaceTemp,
    /// Copy the downloaded Workspace somewhere permanent, then open that.
    SaveWorkspacePermanently,
    /// Save, step 1: connect to the repo and list its branches and tags.
    SaveConnect,
    /// Save, step 2: accept the in-repo paths and move on to the target ref.
    SaveChoosePaths,
    /// Save, step 3: accept the branch/tag and move on to the commit message.
    SaveChooseTarget,
    /// Save, final step: build the payload and push it.
    Save,
}

#[derive(Clone, Copy)]
struct UiColors {
    dim: egui::Color32,
    accent: egui::Color32,
    err: egui::Color32,
}

impl RemoteUi {
    /// Begin loading a collection/environment from a git remote.
    pub fn open_load(&mut self) {
        self.flow = Some(Flow::Load(LoadFlow::new(LoadTarget::File)));
    }

    /// Begin loading a whole Workspace (every file matching a chosen type
    /// filter) from a git remote.
    pub fn open_load_workspace(&mut self) {
        self.flow = Some(Flow::Load(LoadFlow::new(LoadTarget::Workspace)));
    }

    /// Begin saving collection `ci` to a git remote.
    pub fn open_save_collection(&mut self, ci: usize) {
        self.flow = Some(Flow::Save(SaveFlow::new(SaveTarget::Collection(ci))));
    }

    /// Begin saving the whole Workspace tab `ci` came from back to the git
    /// remote it was downloaded from.
    pub fn open_save_workspace(&mut self, ci: usize) {
        self.flow = Some(Flow::Save(SaveFlow::new(SaveTarget::Workspace(ci))));
    }

    /// Begin saving the open report to a git remote.
    pub fn open_save_report(&mut self) {
        self.flow = Some(Flow::Save(SaveFlow::new(SaveTarget::Report)));
    }

    fn is_open(&self) -> bool {
        self.flow.is_some()
    }
}

/// Render the Git dialog (if any) and drive its background work. Call once per
/// frame from the app's `ui`.
pub fn show(app: &mut GuiApp, ctx: &egui::Context) {
    if !app.remote.is_open() {
        return;
    }

    let Some(mut flow) = app.remote.flow.take() else {
        return;
    };
    if let Flow::Save(save) = &mut flow {
        save.ensure_initialized(app);
        save.remember_step();
    }

    if flow.poll(app) {
        return;
    }

    let colors = UiColors {
        dim: app.theme.dim,
        accent: app.theme.accent,
        err: app.theme.err,
    };
    let title = flow.title(&app.strings);
    let mut action = UiAction::None;

    let strings = &app.strings;
    let dismissed = super::widgets::dialog(ctx, title, Some(460.0), |ui| {
        action = draw_flow(ui, &mut flow, colors, strings);
    })
    .dismissed;
    // The ✕ and Escape are the Cancel button by another name.
    if dismissed {
        action = UiAction::Cancel;
    }

    let mut close = false;
    match action {
        UiAction::None => {}
        UiAction::Cancel => {
            // A cancel after the download has landed (only reachable if a
            // future step adds a cancel button past `WorkspaceStorage`) must
            // not strand the checkout on disk with nothing referencing it.
            if let Flow::Load(load) = &mut flow
                && let Some(root) = load.ws_root.take()
            {
                git_remote::cleanup(&root);
            }
            close = true;
        }
        UiAction::Connect => {
            if let Flow::Load(load) = &mut flow {
                start_list_refs(load, &app.strings);
            }
        }
        UiAction::SaveConnect => {
            if let Flow::Save(save) = &mut flow {
                save.clear_errors();
                if let Some(f) = save.flow.as_mut() {
                    f.submit_connect(&app.strings);
                }
            }
        }
        UiAction::SaveChoosePaths => {
            if let Flow::Save(save) = &mut flow {
                save.clear_errors();
                if let Some(f) = save.flow.as_mut() {
                    f.submit_paths(&app.strings);
                }
            }
        }
        UiAction::SaveChooseTarget => {
            if let Flow::Save(save) = &mut flow {
                save.clear_errors();
                if let Some(f) = save.flow.as_mut() {
                    f.submit_target();
                }
            }
        }
        UiAction::BackToConnect => match &mut flow {
            Flow::Load(load) => {
                load.flow.back_to_connect();
                load.local_error = None;
            }
            Flow::Save(save) => {
                save.clear_errors();
                if let Some(f) = save.flow.as_mut() {
                    f.back_to_connect();
                }
            }
        },
        UiAction::BrowseFiles => {
            if let Flow::Load(load) = &mut flow {
                start_list_files(load, &app.strings);
            }
        }
        UiAction::BackToRefs => {
            if let Flow::Load(load) = &mut flow {
                load.flow.back_to_refs();
                load.local_error = None;
            }
        }
        UiAction::LoadFile => {
            if let Flow::Load(load) = &mut flow {
                start_checkout(load, &app.strings);
            }
        }
        UiAction::DownloadWorkspace => {
            if let Flow::Load(load) = &mut flow {
                start_workspace_checkout(load, &app.strings);
            }
        }
        UiAction::KeepWorkspaceTemp => {
            if let Flow::Load(load) = &mut flow
                && load.finish_workspace(app)
            {
                return;
            }
        }
        UiAction::SaveWorkspacePermanently => {
            if let Flow::Load(load) = &mut flow {
                ask_where_to_save_workspace(load, app);
            }
        }
        UiAction::Save => {
            if let Flow::Save(save) = &mut flow {
                save.clear_errors();
                start_save(save, app);
            }
        }
    }

    if close {
        return;
    }

    if flow.is_busy() {
        ctx.request_repaint_after(REPAINT_WHILE_BUSY);
    }
    app.remote.flow = Some(flow);
}

impl Flow {
    fn title(&self, s: &Strings) -> &'static str {
        match self {
            Flow::Load(load) => match load.target {
                LoadTarget::File => s.gui_git_load_title,
                LoadTarget::Workspace => s.gui_git_load_workspace_title,
            },
            Flow::Save(save) => match save.target {
                SaveTarget::Collection(_) => s.gui_git_save_collection_title,
                SaveTarget::Workspace(_) => s.gui_git_save_workspace_title,
                SaveTarget::Report => s.gui_git_save_report_title,
            },
        }
    }

    fn is_busy(&self) -> bool {
        match self {
            Flow::Load(load) => load.is_busy(),
            Flow::Save(save) => save.is_busy(),
        }
    }

    fn poll(&mut self, app: &mut GuiApp) -> bool {
        match self {
            Flow::Load(load) => load.poll(app),
            Flow::Save(save) => save.poll(app),
        }
    }
}

fn draw_flow(ui: &mut egui::Ui, flow: &mut Flow, colors: UiColors, s: &Strings) -> UiAction {
    match flow {
        Flow::Load(load) => draw_load(ui, load, colors, s),
        Flow::Save(save) => draw_save(ui, save, colors, s),
    }
}

fn draw_busy_and_error(
    ui: &mut egui::Ui,
    busy_label: Option<&'static str>,
    error: Option<&str>,
    colors: UiColors,
) {
    if let Some(label) = busy_label {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.colored_label(colors.dim, label);
        });
        ui.add_space(6.0);
    }
    if let Some(error) = error {
        ui.colored_label(colors.err, error);
        ui.add_space(6.0);
    }
}

fn draw_load(ui: &mut egui::Ui, load: &mut LoadFlow, colors: UiColors, s: &Strings) -> UiAction {
    let busy = load.is_busy();
    let busy_label = load.busy_label(s);
    let error = load.error().map(str::to_string);
    draw_busy_and_error(ui, busy_label, error.as_deref(), colors);

    match load.step() {
        LoadStep::Connect => draw_load_connect(ui, load, busy, colors, s),
        LoadStep::PickRef => draw_load_pick_ref(ui, load, busy, colors, s),
        LoadStep::PickFile => draw_load_pick_file(ui, load, busy, colors, s),
        LoadStep::PickWorkspaceFilter => draw_load_workspace_filter(ui, load, busy, colors, s),
        LoadStep::WorkspaceStorage => draw_load_workspace_storage(ui, load, busy, colors, s),
    }
}

/// Workspace load, step 3: pick which file types to download. Nothing has been
/// fetched from the repo yet beyond its *listing*, so this is the last chance
/// to keep a repo full of large, unrelated files off the user's disk.
fn draw_load_workspace_filter(
    ui: &mut egui::Ui,
    load: &mut LoadFlow,
    busy: bool,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;

    if let Some(sha) = load.flow.commit_sha() {
        ui.colored_label(
            colors.dim,
            format!("{} {}", s.gui_git_fetched_at, short_sha(sha)),
        );
    }
    ui.colored_label(colors.dim, s.gui_git_ws_pick_filter);
    ui.add_space(4.0);
    ui.add_enabled_ui(!busy, |ui| {
        for choice in WorkspaceGitFilter::ALL {
            ui.radio_value(&mut load.ws_filter, choice, choice.label(s));
        }
    });

    let matched = load.workspace_match_count();
    ui.add_space(4.0);
    ui.colored_label(
        colors.dim,
        s.gui_git_ws_match_count
            .replace("{n}", &matched.to_string())
            .replace("{total}", &load.flow.all_files().len().to_string()),
    );
    if matched == 0 {
        ui.colored_label(colors.err, s.gui_git_err_ws_no_matches);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && matched > 0,
                egui::Button::new(s.gui_git_ws_download),
            )
            .clicked()
        {
            action = UiAction::DownloadWorkspace;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_back))
            .clicked()
        {
            action = UiAction::BackToRefs;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_cancel))
            .clicked()
        {
            action = UiAction::Cancel;
        }
    });

    action
}

/// Workspace load, step 4: the files are on disk in a throwaway folder. Keep
/// them there (nothing is ever cleaned up automatically, so the folder lives as
/// long as the tab does), or copy them somewhere permanent right now.
fn draw_load_workspace_storage(
    ui: &mut egui::Ui,
    load: &mut LoadFlow,
    busy: bool,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;

    ui.colored_label(colors.accent, s.gui_git_ws_storage_title);
    ui.add_space(4.0);
    ui.colored_label(colors.dim, s.git_workspace_storage_q);
    ui.add_space(6.0);
    ui.colored_label(colors.accent, s.gui_git_ws_folder_name);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut load.ws_name).desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new(s.git_workspace_storage_choose))
            .clicked()
        {
            action = UiAction::SaveWorkspacePermanently;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.git_workspace_storage_temp))
            .clicked()
        {
            action = UiAction::KeepWorkspaceTemp;
        }
    });

    action
}

fn draw_load_connect(
    ui: &mut egui::Ui,
    load: &mut LoadFlow,
    busy: bool,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;

    ui.colored_label(colors.accent, s.gui_git_repo_url);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut load.flow.url).desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.colored_label(colors.accent, s.gui_git_token);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut load.flow.token).desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && !load.flow.url.trim().is_empty(),
                egui::Button::new(s.gui_git_connect),
            )
            .clicked()
        {
            action = UiAction::Connect;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_cancel))
            .clicked()
        {
            action = UiAction::Cancel;
        }
    });

    action
}

fn draw_load_pick_ref(
    ui: &mut egui::Ui,
    load: &mut LoadFlow,
    busy: bool,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;

    ui.colored_label(colors.dim, s.gui_git_pick_ref);
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut load.ref_kind,
                RefKind::Branch,
                format!(
                    "{} ({})",
                    s.gui_git_branches,
                    load.flow.refs().branches.len()
                ),
            );
            ui.radio_value(
                &mut load.ref_kind,
                RefKind::Tag,
                format!("{} ({})", s.gui_git_tags, load.flow.refs().tags.len()),
            );
        });

        let choices = match load.ref_kind {
            RefKind::Branch => &load.flow.refs().branches,
            RefKind::Tag => &load.flow.refs().tags,
        };
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for (idx, name) in choices.iter().enumerate() {
                    let selected = match load.ref_kind {
                        RefKind::Branch => load.selected_branch == idx,
                        RefKind::Tag => load.selected_tag == idx,
                    };
                    if super::widgets::selectable(ui, selected, name.as_str()).clicked() {
                        match load.ref_kind {
                            RefKind::Branch => load.selected_branch = idx,
                            RefKind::Tag => load.selected_tag = idx,
                        }
                    }
                }
            });
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && load.selected_ref_name().is_some(),
                egui::Button::new(s.gui_git_browse_files),
            )
            .clicked()
        {
            action = UiAction::BrowseFiles;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_back))
            .clicked()
        {
            action = UiAction::BackToConnect;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_cancel))
            .clicked()
        {
            action = UiAction::Cancel;
        }
    });

    action
}

fn draw_load_pick_file(
    ui: &mut egui::Ui,
    load: &mut LoadFlow,
    busy: bool,
    colors: UiColors,
    s: &Strings,
) -> UiAction {
    let mut action = UiAction::None;

    if let Some(sha) = load.flow.commit_sha() {
        ui.colored_label(
            colors.dim,
            format!("{} {}", s.gui_git_fetched_at, short_sha(sha)),
        );
    }
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.label(s.gui_git_filter);
            ui.add(egui::TextEdit::singleline(&mut load.filter).desired_width(240.0));
            ui.checkbox(&mut load.show_all_files, s.gui_git_show_all_files);
        });

        let visible = load.visible_files();
        if visible.is_empty() {
            ui.colored_label(colors.dim, s.gui_git_no_files);
        } else {
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for path in visible.iter() {
                        let selected = load.selected_path.as_deref() == Some(path.as_str());
                        if super::widgets::selectable(ui, selected, path.as_str()).clicked() {
                            load.selected_path = Some(path.clone());
                        }
                    }
                });
        }
    });

    if !load.flow.has_repo() {
        ui.colored_label(colors.dim, s.gui_git_checkout_gone);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && load.flow.has_repo() && load.selected_path.is_some(),
                egui::Button::new(s.gui_git_load),
            )
            .clicked()
        {
            action = UiAction::LoadFile;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_git_back))
            .clicked()
        {
            action = UiAction::BackToRefs;
        }
        if ui
            .add_enabled(!busy, egui::Button::new(s.gui_cancel))
            .clicked()
        {
            action = UiAction::Cancel;
        }
    });

    action
}

fn draw_save(ui: &mut egui::Ui, save: &mut SaveFlow, colors: UiColors, s: &Strings) -> UiAction {
    let busy = save.is_busy();
    let busy_label = save.busy_label(s);
    let error = save.error().map(str::to_string);
    draw_busy_and_error(ui, busy_label, error.as_deref(), colors);

    if save.blocked {
        // There is nothing left to save; only the way out is offered.
        return if ui.button(s.gui_cancel).clicked() {
            UiAction::Cancel
        } else {
            UiAction::None
        };
    }

    let step = save.step();
    let Some(flow) = save.flow.as_mut() else {
        return UiAction::None;
    };
    let mut action = UiAction::None;

    match step {
        SaveStep::Connect => {
            ui.colored_label(colors.accent, s.gui_git_repo_url);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut flow.url).desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.colored_label(colors.accent, s.gui_git_token);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut flow.token).desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy && !flow.url.trim().is_empty(),
                        egui::Button::new(s.gui_git_connect),
                    )
                    .clicked()
                {
                    action = UiAction::SaveConnect;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(s.gui_cancel))
                    .clicked()
                {
                    action = UiAction::Cancel;
                }
            });
        }
        SaveStep::ChoosePaths => {
            ui.colored_label(colors.accent, s.gui_git_path);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut flow.path).desired_width(f32::INFINITY),
            );
            if save.env.is_some() && flow.source.can_include_env() {
                ui.add_space(6.0);
                ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut flow.include_env, s.git_save_include_env_label),
                );
                if flow.include_env {
                    ui.add_space(4.0);
                    ui.colored_label(colors.accent, s.git_save_env_path_label);
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut flow.env_path).desired_width(f32::INFINITY),
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy && !flow.path.trim().is_empty(),
                        egui::Button::new(s.gui_git_next),
                    )
                    .clicked()
                {
                    action = UiAction::SaveChoosePaths;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(s.gui_git_back))
                    .clicked()
                {
                    action = UiAction::BackToConnect;
                }
            });
        }
        SaveStep::ChooseTarget => {
            // Branch or tag is the one choice with real consequences here — a
            // tag is never overwritten — so it is a visible toggle rather than
            // something inferred from the name.
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut flow.target_kind,
                    SaveTargetKind::Branch,
                    s.gui_git_branches,
                );
                ui.selectable_value(&mut flow.target_kind, SaveTargetKind::Tag, s.gui_git_tags);
            });
            ui.add_space(4.0);
            ui.colored_label(
                colors.accent,
                if flow.target_kind == SaveTargetKind::Branch {
                    s.gui_git_branch
                } else {
                    s.gui_git_tag
                },
            );
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut flow.target_name).desired_width(f32::INFINITY),
            );

            // Offer the branches that already exist, so appending to one is a
            // click rather than an exact-spelling exercise.
            let branches = flow.refs().branches.clone();
            if flow.target_kind == SaveTargetKind::Branch && !branches.is_empty() {
                ui.add_space(4.0);
                egui::ComboBox::from_id_salt("git_save_existing_branch")
                    .selected_text(s.gui_git_existing_branch)
                    .show_ui(ui, |ui| {
                        for (i, branch) in branches.iter().enumerate() {
                            if ui
                                .selectable_label(save.selected_branch == i, branch)
                                .clicked()
                            {
                                save.selected_branch = i;
                                flow.target_name = branch.clone();
                            }
                        }
                    });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy && !flow.target_name.trim().is_empty(),
                        egui::Button::new(s.gui_git_next),
                    )
                    .clicked()
                {
                    action = UiAction::SaveChooseTarget;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(s.gui_git_back))
                    .clicked()
                {
                    action = UiAction::BackToConnect;
                }
            });
        }
        SaveStep::CommitMessage => {
            ui.colored_label(colors.accent, s.gui_git_commit_message);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut flow.message).desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy && !flow.message.trim().is_empty(),
                        egui::Button::new(s.gui_git_save),
                    )
                    .clicked()
                {
                    action = UiAction::Save;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(s.gui_cancel))
                    .clicked()
                {
                    action = UiAction::Cancel;
                }
            });
        }
        SaveStep::Done => {
            ui.colored_label(colors.accent, s.gui_git_saved);
            ui.add_space(8.0);
            if ui.button(s.gui_close).clicked() {
                action = UiAction::Cancel;
            }
        }
    }

    action
}

fn start_list_refs(load: &mut LoadFlow, s: &Strings) {
    if nonblank(load.flow.url.trim()).is_none() {
        load.local_error = Some(s.gui_git_err_url_required.to_string());
        return;
    }
    load.clear_errors();
    load.flow.connect();
}

fn start_list_files(load: &mut LoadFlow, s: &Strings) {
    let (Some(name), Some(gitref)) = (load.selected_ref_name(), load.selected_gitref()) else {
        load.local_error = Some(s.gui_git_err_pick_ref_first.to_string());
        return;
    };
    let choice = RefChoice {
        label: name.to_string(),
        gitref,
    };
    load.clear_errors();
    load.filter.clear();
    load.show_all_files = false;
    load.selected_path = None;
    load.flow.choose_ref(choice);
}

fn start_checkout(load: &mut LoadFlow, s: &Strings) {
    let Some(path) = load.selected_path.clone() else {
        load.local_error = Some(s.gui_git_err_pick_file.to_string());
        return;
    };
    load.clear_errors();
    load.flow.choose_file(path);
}

/// Final save step: look up whatever the dialog was opened on — it may have
/// been closed while the dialog was up — hand it to the shared flow to turn
/// into files, and let the flow push. The lookup is the only part that has to
/// live here; the validation and the push are shared with the terminal UI.
fn start_save(save: &mut SaveFlow, app: &mut GuiApp) {
    let Some(flow) = save.flow.as_ref() else {
        return;
    };
    let payload = match &flow.source {
        SaveSource::Workspace { root, .. } => CoreSaveFlow::workspace_payload(root, &app.strings),
        SaveSource::Collection { ci } => match app.session.collections.get(*ci) {
            Some(col) => flow.collection_payload(col, save.env.as_ref(), &app.strings),
            None => Err(app.strings.git_save_source_gone.to_string()),
        },
        SaveSource::Report { .. } => match app.report_editor.as_ref() {
            Some(editor) => Ok(flow.report_payload(&editor.report)),
            None => Err(app.strings.git_save_source_gone.to_string()),
        },
    };
    if let Some(flow) = save.flow.as_mut() {
        flow.submit_message(payload);
    }
}

fn start_workspace_checkout(load: &mut LoadFlow, s: &Strings) {
    if load.workspace_match_count() == 0 {
        load.local_error = Some(s.gui_git_err_ws_no_matches.to_string());
        return;
    }
    load.clear_errors();
    let filter = load.ws_filter;
    load.flow.choose_workspace_filter(filter);
}

/// Copy the just-downloaded Workspace out of its temp folder into a permanent
/// location the user picks, and repoint the flow at the copy. Returns `false`
/// (leaving the storage step open, with an error where it's the user's to fix)
/// if the user cancelled the picker or the copy failed — the temp folder is
/// kept in that case rather than losing the download outright.
fn ask_where_to_save_workspace(load: &mut LoadFlow, app: &mut GuiApp) {
    // Both answers are the user's to fix, so they are checked before a dialog
    // is opened rather than after they have chosen a folder for nothing.
    if load.ws_root.is_none() {
        load.local_error = Some(app.strings.gui_git_err_browse_again.to_string());
        return;
    }
    if nonblank(&load.ws_name).is_none() {
        load.local_error = Some(app.strings.gui_git_err_ws_name_required.to_string());
        return;
    }
    let title = app.strings.git_workspace_storage_choose;
    let dir = app.session.last_browse_dir.clone();
    app.request_pick(
        super::filepick::PickKind::Folder,
        title,
        dir.as_deref(),
        super::menu::PickAction::GitWorkspaceDir,
    );
}

/// Copy the downloaded workspace into the folder the dialog named, and finish
/// the flow if that worked.
///
/// A cancel leaves the storage step open with the workspace still temporary:
/// backing out of the folder chooser is not a decision to throw the download
/// away.
pub(super) fn apply_picked_workspace_dir(app: &mut GuiApp, picked: Option<std::path::PathBuf>) {
    let Some(parent) = picked else {
        return; // cancelled — stay on the question, keep it temporary
    };
    let Some(mut flow) = app.remote.flow.take() else {
        return; // the dialog outlived its wizard
    };
    if let Flow::Load(load) = &mut flow
        && save_workspace_into(load, app, parent)
        && load.finish_workspace(app)
    {
        return; // finished: the flow is closed, not put back
    }
    app.remote.flow = Some(flow);
}

fn save_workspace_into(load: &mut LoadFlow, app: &mut GuiApp, parent: std::path::PathBuf) -> bool {
    let Some(source) = load.ws_root.clone() else {
        load.local_error = Some(app.strings.gui_git_err_browse_again.to_string());
        return false;
    };
    let Some(name) = nonblank(&load.ws_name) else {
        load.local_error = Some(app.strings.gui_git_err_ws_name_required.to_string());
        return false;
    };

    // Copy into `<chosen folder>/<name>` rather than straight into the chosen
    // folder, so picking an existing folder full of unrelated files can never
    // mix the workspace into it.
    let dest = parent.join(&name);
    if dest.exists() {
        load.local_error = Some(app.strings.gui_git_err_ws_exists.to_string());
        return false;
    }
    if let Err(e) = crate::workspace::copy_dir_all(&source, &dest) {
        load.local_error = Some(e.to_string());
        return false;
    }

    // The copy is the workspace now; the temp download has served its purpose.
    git_remote::cleanup(&source);
    app.session.last_browse_dir = Some(parent);
    app.session.status = Some(Status::WorkspaceSaved);
    load.ws_root = Some(dest);
    true
}

fn nonblank(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_default_load_file(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl")
        || e.eq_ignore_ascii_case("vars")
        // Postman exports both collections and environments as JSON.
        || e.eq_ignore_ascii_case("json"))
}

fn is_vars_file(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    matches!(ext, Some(e) if e.eq_ignore_ascii_case("vars"))
}

fn name_from_repo_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// A tab name for a Workspace loaded from `url`: the repository's own name
/// (`…/team/api-tests.git` → `api-tests`). The folder it was downloaded into is
/// a meaningless temp path, so the URL is the only human-readable source.
fn file_stem_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .find(|seg| !seg.is_empty())
        .unwrap_or(trimmed);
    let stem = last.strip_suffix(".git").unwrap_or(last).trim();
    if stem.is_empty() {
        "workspace".to_string()
    } else {
        stem.to_string()
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn remember_git_url(session: &mut crate::session::Session, url: &str) {
    let Some(url) = nonblank(url) else {
        return;
    };
    session.recent_git_urls.retain(|known| known != &url);
    session.recent_git_urls.insert(0, url);
    session.recent_git_urls.truncate(12);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::Collection;
    use crate::hurl::HurlEntry;
    use crate::i18n::Language;
    use crate::remote_flow::Step;
    use crate::session::Session;

    fn git(args: &[&str], dir: &std::path::Path) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git is on PATH");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A real (bare) repository on disk, so the flow can be driven through its
    /// actual threads and git invocations rather than a stubbed transport.
    fn seed_bare_repo() -> (String, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "paperboy-gui-remote-test-{}-{nanos}",
            std::process::id()
        ));
        let bare = base.join("bare.git");
        let work = base.join("work");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&["init", "--bare", "-q", "."], &bare);
        git(&["init", "-q"], &work);
        git(&["checkout", "-q", "-b", "main"], &work);
        git(&["config", "user.name", "Seed"], &work);
        git(&["config", "user.email", "seed@test"], &work);
        std::fs::write(work.join("api.hurl"), "GET https://example.com/a\n").unwrap();
        std::fs::write(work.join("legacy.json"), "{}").unwrap();
        std::fs::write(work.join("dev.vars"), "HOST=example.com\n").unwrap();
        std::fs::write(work.join("big.bin"), "x".repeat(512)).unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "-q", "-m", "seed"], &work);
        git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
        git(&["push", "-q", "origin", "main"], &work);
        (bare.to_str().unwrap().to_string(), base)
    }

    fn main_branch_index(load: &LoadFlow) -> usize {
        load.flow
            .refs()
            .branches
            .iter()
            .position(|b| b == "main")
            .expect("the seeded repo has a main branch")
    }

    /// Stand in for the egui frame loop: poll until the flow leaves the step it
    /// is on, or until `poll` reports the wizard is finished and should close.
    /// Fails loudly rather than hanging if neither ever happens.
    ///
    /// Returns whether the wizard asked to close.
    fn pump_until_past(load: &mut LoadFlow, app: &mut GuiApp, step: LoadStep) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if load.poll(app) {
                return true;
            }
            if load.step() != step || load.error().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "stuck on {step:?} (error: {:?})",
                load.error()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(load.error(), None, "flow failed on {step:?}");
        false
    }

    fn seeded(target: LoadTarget, files: &[&str]) -> LoadFlow {
        let mut load = LoadFlow::new(target);
        load.flow = RemoteFlow::seed(
            load.flow.kind,
            "https://example.test/repo.git",
            if target == LoadTarget::Workspace {
                Step::PickWorkspaceFilter
            } else {
                Step::PickFile
            },
            files.iter().map(|f| f.to_string()).collect(),
            Some(std::env::temp_dir().join("pb-gui-remote-test")),
        );
        load
    }

    /// The GUI decides whether a fetched file is a collection or an environment
    /// from its content, so unlike the terminal UI it must offer both kinds in
    /// the picker rather than narrowing to one.
    #[test]
    fn a_file_load_offers_collections_and_environments_together() {
        let load = seeded(
            LoadTarget::File,
            &["api.hurl", "envs/dev.vars", "postman.json", "README.md"],
        );
        let visible = load.visible_files();
        assert!(visible.contains(&"api.hurl".to_string()));
        assert!(visible.contains(&"envs/dev.vars".to_string()));
        assert!(visible.contains(&"postman.json".to_string()));
        assert!(
            !visible.contains(&"README.md".to_string()),
            "unrelated files are hidden until 'show all' is ticked"
        );
    }

    /// A repo that names its files unusually must not strand the user with an
    /// empty picker.
    #[test]
    fn showing_all_files_reveals_everything_the_repo_has() {
        let mut load = seeded(LoadTarget::File, &["notes.md", "script.sh"]);
        assert!(load.visible_files().is_empty());
        load.show_all_files = true;
        assert_eq!(load.visible_files().len(), 2);
    }

    /// The picker asks for its file list on every frame, so the list is
    /// memoised: the same answer comes back as the same allocation until one of
    /// the things it was computed from changes.
    #[test]
    fn the_file_list_is_reused_until_something_it_depends_on_changes() {
        let mut load = seeded(LoadTarget::File, &["api/Health.hurl", "api/orders.hurl"]);

        let first = load.visible_files();
        assert!(
            std::rc::Rc::ptr_eq(&first, &load.visible_files()),
            "a second frame is served the list already in hand"
        );

        load.filter = "health".to_string();
        let filtered = load.visible_files();
        assert!(
            !std::rc::Rc::ptr_eq(&first, &filtered),
            "typing in the filter box recomputes it"
        );
        assert_eq!(*filtered, vec!["api/Health.hurl"]);

        load.show_all_files = true;
        assert!(
            !std::rc::Rc::ptr_eq(&filtered, &load.visible_files()),
            "so does the show-all checkbox"
        );

        // A different repo, same number of files and the same filter: the cache
        // must not serve the previous repo's paths.
        load.filter.clear();
        load.show_all_files = false;
        let before = load.visible_files();
        load.flow = RemoteFlow::seed(
            load.flow.kind,
            "https://example.test/other.git",
            Step::PickFile,
            vec!["other/a.hurl".to_string(), "other/b.hurl".to_string()],
            Some(std::env::temp_dir().join("pb-gui-remote-test")),
        );
        assert_ne!(*load.visible_files(), *before, "a fetch invalidates it");
    }

    #[test]
    fn the_file_filter_is_case_insensitive() {
        let mut load = seeded(LoadTarget::File, &["api/Health.hurl", "api/orders.hurl"]);
        load.filter = "HEALTH".to_string();
        assert_eq!(*load.visible_files(), vec!["api/Health.hurl"]);
    }

    /// The step shown is derived from the shared flow, so the GUI cannot drift
    /// out of step with the state machine driving it.
    #[test]
    fn the_step_shown_follows_the_shared_flow() {
        let mut load = LoadFlow::new(LoadTarget::File);
        assert_eq!(load.step(), LoadStep::Connect);
        load.flow.seed_refs(&["main"], &["v1"]);
        assert_eq!(load.step(), LoadStep::PickRef);
    }

    /// A workspace load asks which file *types* to download and never picks a
    /// single file — nothing is fetched until that is answered.
    #[test]
    fn a_workspace_load_asks_for_a_type_filter_rather_than_a_file() {
        let load = seeded(LoadTarget::Workspace, &["a.hurl", "b.json", "big/blob.bin"]);
        assert_eq!(load.step(), LoadStep::PickWorkspaceFilter);
        assert_eq!(load.workspace_match_count(), 2, "the .bin is left behind");
    }

    /// Downloading nothing at all is a mistake worth naming, rather than a
    /// silent no-op that looks like the button is broken.
    #[test]
    fn a_filter_matching_nothing_is_reported_instead_of_downloading_nothing() {
        let s = Strings::for_language(&Language::English);
        let mut load = seeded(LoadTarget::Workspace, &["big/blob.bin", "readme.md"]);
        start_workspace_checkout(&mut load, &s);
        assert_eq!(load.error(), Some(s.gui_git_err_ws_no_matches));
        assert!(!load.is_busy(), "nothing was fetched");
    }

    #[test]
    fn connecting_without_a_url_is_reported() {
        let s = Strings::for_language(&Language::English);
        let mut load = LoadFlow::new(LoadTarget::File);
        start_list_refs(&mut load, &s);
        assert_eq!(load.error(), Some(s.gui_git_err_url_required));
        assert_eq!(load.step(), LoadStep::Connect);
    }

    /// Errors the GUI raises itself and errors coming back from git are shown
    /// the same way, so neither can hide the other.
    #[test]
    fn an_error_from_either_source_is_shown() {
        let mut load = LoadFlow::new(LoadTarget::File);
        assert_eq!(load.error(), None);
        load.flow.fail("remote hung up".into());
        assert_eq!(load.error(), Some("remote hung up"));
        load.clear_errors();
        assert_eq!(load.error(), None);
    }

    /// Going back a step must not throw away the ref listing that was already
    /// fetched — picking a different branch shouldn't mean a second round trip.
    #[test]
    fn going_back_to_the_refs_keeps_them() {
        let mut load = seeded(LoadTarget::File, &["a.hurl"]);
        load.flow.seed_refs(&["main", "dev"], &[]);
        load.flow.back_to_refs();
        assert_eq!(load.step(), LoadStep::PickRef);
        assert_eq!(load.flow.refs().branches.len(), 2);
        assert!(!load.flow.has_repo(), "the checkout is released");
    }

    #[test]
    fn the_selected_ref_becomes_a_fully_qualified_git_ref() {
        let mut load = LoadFlow::new(LoadTarget::File);
        load.flow.seed_refs(&["main"], &["v1.2"]);
        assert_eq!(load.selected_gitref().as_deref(), Some("refs/heads/main"));
        load.ref_kind = RefKind::Tag;
        assert_eq!(load.selected_gitref().as_deref(), Some("refs/tags/v1.2"));
    }

    /// The whole single-file path against a real repository: connect, list
    /// refs, list files, fetch one, and hand it to the session. This is the
    /// test the GUI lacked when its copy of the flow drifted from the terminal
    /// UI's, so it deliberately exercises the threads rather than seeding state.
    #[test]
    fn a_file_can_be_loaded_from_a_real_repository_end_to_end() {
        let (repo_url, base) = seed_bare_repo();
        let mut app = GuiApp::for_test(Session::default());
        let s = Strings::for_language(&Language::English);
        let mut load = LoadFlow::new(LoadTarget::File);
        load.flow.url = repo_url.clone();

        start_list_refs(&mut load, &s);
        pump_until_past(&mut load, &mut app, LoadStep::Connect);
        assert_eq!(load.step(), LoadStep::PickRef);
        assert!(load.flow.refs().branches.iter().any(|b| b == "main"));

        load.selected_branch = main_branch_index(&load);
        start_list_files(&mut load, &s);
        pump_until_past(&mut load, &mut app, LoadStep::PickRef);
        assert_eq!(load.step(), LoadStep::PickFile);
        assert!(load.visible_files().contains(&"api.hurl".to_string()));
        assert!(
            !load.visible_files().contains(&"big.bin".to_string()),
            "unrelated files stay hidden"
        );

        load.selected_path = Some("api.hurl".to_string());
        start_checkout(&mut load, &s);
        assert!(
            pump_until_past(&mut load, &mut app, LoadStep::PickFile),
            "the wizard closes itself once the file is loaded"
        );

        assert!(
            !app.session.collections.is_empty(),
            "the fetched collection reached the session"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The workspace path: the filter decides what is downloaded, and the
    /// result waits on the "keep or save" step instead of opening straight away.
    #[test]
    fn a_workspace_can_be_downloaded_from_a_real_repository_end_to_end() {
        let (repo_url, base) = seed_bare_repo();
        let mut app = GuiApp::for_test(Session::default());
        let s = Strings::for_language(&Language::English);
        let mut load = LoadFlow::new(LoadTarget::Workspace);
        load.flow.url = repo_url.clone();

        start_list_refs(&mut load, &s);
        pump_until_past(&mut load, &mut app, LoadStep::Connect);
        load.selected_branch = main_branch_index(&load);
        start_list_files(&mut load, &s);
        pump_until_past(&mut load, &mut app, LoadStep::PickRef);
        assert_eq!(load.step(), LoadStep::PickWorkspaceFilter);

        load.ws_filter = WorkspaceGitFilter::HurlAndJson;
        start_workspace_checkout(&mut load, &s);
        pump_until_past(&mut load, &mut app, LoadStep::PickWorkspaceFilter);

        assert_eq!(
            load.step(),
            LoadStep::WorkspaceStorage,
            "the download pauses to ask where it should live"
        );
        let root = load.ws_root.clone().expect("a folder was downloaded");
        assert!(root.join("api.hurl").is_file());
        assert!(root.join("legacy.json").is_file());
        assert!(
            !root.join("big.bin").exists() && !root.join("dev.vars").exists(),
            "the chosen filter left everything that isn't a collection behind"
        );

        assert!(load.finish_workspace(&mut app));
        assert!(
            app.session
                .collections
                .iter()
                .any(|c| c.workspace_root.is_some()),
            "the workspace tab was opened"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
    /// Stand in for the egui frame loop for a save, mirroring
    /// [`pump_until_past`]. Returns whether the push finished.
    fn pump_save_until_past(save: &mut SaveFlow, app: &mut GuiApp, step: SaveStep) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if save.poll(app) {
                return true;
            }
            if save.step() != step || save.error().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "stuck on {step:?} (error: {:?})",
                save.error()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(save.error(), None, "flow failed on {step:?}");
        false
    }

    /// Wait for the branch/tag listing, which is fetched in the background
    /// while the target step is already on screen.
    fn pump_save_until_refs(save: &mut SaveFlow, app: &mut GuiApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while save.is_busy() {
            assert!(!save.poll(app), "the flow finished before the refs arrived");
            assert!(
                std::time::Instant::now() < deadline,
                "the ref listing never arrived (error: {:?})",
                save.error()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(save.error(), None, "listing the refs failed");
    }

    /// A session whose *only* tab is a collection with one request, so
    /// `Collection(0)` is unambiguous.
    fn session_with_collection() -> Session {
        let mut session = Session::default();
        session.collections.clear();
        let col = Collection::new(
            "Demo".to_string(),
            vec![HurlEntry {
                method: "GET".to_string(),
                url: "https://example.com/a".to_string(),
                // Unsaved, as a tab about to be pushed would be.
                user_added: true,
                ..Default::default()
            }],
        );
        session.collections.push(col);
        session
    }

    /// A save opened on a tab that has since been closed must say so and offer
    /// no way to push, rather than failing somewhere deep in the worker.
    #[test]
    fn saving_a_collection_that_is_gone_is_blocked_up_front() {
        let app = GuiApp::for_test(Session::default());
        let gone = app.session.collections.len();
        let mut save = SaveFlow::new(SaveTarget::Collection(gone));
        save.ensure_initialized(&app);
        assert!(save.blocked);
        assert_eq!(
            save.error(),
            Some(app.strings.gui_git_err_collection_missing)
        );
        assert!(save.flow.is_none(), "no push machinery is built at all");
    }

    /// Every save target gets its own dialog title. They all used to read
    /// "Save collection to Git", so pushing a workspace or a report announced
    /// itself as something it wasn't.
    #[test]
    fn each_save_target_titles_its_own_dialog() {
        let s = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
        let title = |t| Flow::Save(SaveFlow::new(t)).title(&s);
        assert_eq!(
            title(SaveTarget::Collection(0)),
            s.gui_git_save_collection_title
        );
        assert_eq!(
            title(SaveTarget::Workspace(0)),
            s.gui_git_save_workspace_title
        );
        assert_eq!(title(SaveTarget::Report), s.gui_git_save_report_title);
    }

    /// Pushing a workspace back needs somewhere to push it to. A tab opened
    /// from a local folder has no remote, so the dialog explains that instead
    /// of presenting an empty URL box that can never work.
    #[test]
    fn saving_a_workspace_that_did_not_come_from_git_is_blocked() {
        let app = GuiApp::for_test(session_with_collection());
        let mut save = SaveFlow::new(SaveTarget::Workspace(0));
        save.ensure_initialized(&app);
        assert!(save.blocked);
        assert_eq!(save.error(), Some(app.strings.gui_git_err_ws_not_from_git));
    }

    /// The dialog's steps are derived from the shared flow rather than tracked
    /// beside it, so the two can't disagree about where the user is.
    #[test]
    fn the_save_step_shown_follows_the_shared_flow() {
        let app = GuiApp::for_test(session_with_collection());
        let mut save = SaveFlow::new(SaveTarget::Collection(0));
        save.ensure_initialized(&app);
        assert_eq!(save.step(), SaveStep::Connect);

        let flow = save.flow.as_mut().unwrap();
        flow.seed_step(CoreStep::ChoosePaths);
        assert_eq!(save.step(), SaveStep::ChoosePaths);
        save.flow
            .as_mut()
            .unwrap()
            .seed_step(CoreStep::ChooseTarget);
        assert_eq!(save.step(), SaveStep::ChooseTarget);
        save.flow
            .as_mut()
            .unwrap()
            .seed_step(CoreStep::CommitMessage);
        assert_eq!(save.step(), SaveStep::CommitMessage);
        save.flow.as_mut().unwrap().seed_step(CoreStep::Done);
        assert_eq!(save.step(), SaveStep::Done);
    }

    /// A failure puts the dialog back on the first step *and* keeps the message
    /// visible, so the user can correct the URL or token and retry.
    #[test]
    fn a_failed_save_returns_to_the_first_step_with_the_reason() {
        let app = GuiApp::for_test(session_with_collection());
        let mut save = SaveFlow::new(SaveTarget::Collection(0));
        save.ensure_initialized(&app);
        save.flow
            .as_mut()
            .unwrap()
            .seed_step(CoreStep::Failed("remote hung up".into()));
        assert_eq!(save.step(), SaveStep::Connect);
        assert_eq!(save.error(), Some("remote hung up"));
        save.clear_errors();
        assert_eq!(save.error(), None);
    }

    /// A path escaping the repository would write outside the checkout when the
    /// commit is applied, so it is refused before any worker starts.
    #[test]
    fn a_save_path_that_escapes_the_repository_is_refused() {
        let app = GuiApp::for_test(session_with_collection());
        let s = Strings::for_language(&Language::English);
        let mut save = SaveFlow::new(SaveTarget::Collection(0));
        save.ensure_initialized(&app);
        let flow = save.flow.as_mut().unwrap();
        flow.seed_step(CoreStep::ChoosePaths);
        save.remember_step();
        let flow = save.flow.as_mut().unwrap();
        flow.path = "../outside.hurl".to_string();
        assert!(!flow.submit_paths(&s));
        assert_eq!(save.step(), SaveStep::ChoosePaths, "the user stays put");
        assert!(save.error().is_some());
    }

    /// The whole save path against a real repository: connect, choose the path
    /// and the branch, and push. This is the counterpart to
    /// `a_file_can_be_loaded_from_a_real_repository_end_to_end`, and the test
    /// the GUI lacked while it carried its own copy of the push.
    #[test]
    fn a_collection_can_be_saved_to_a_real_repository_end_to_end() {
        let (repo_url, base) = seed_bare_repo();
        let mut app = GuiApp::for_test(session_with_collection());
        let s = Strings::for_language(&Language::English);

        let mut save = SaveFlow::new(SaveTarget::Collection(0));
        save.ensure_initialized(&app);
        let flow = save.flow.as_mut().unwrap();
        flow.url = repo_url.clone();
        flow.submit_connect(&s);
        assert_eq!(save.step(), SaveStep::ChoosePaths);

        let flow = save.flow.as_mut().unwrap();
        flow.path = "demo.hurl".to_string();
        assert!(flow.submit_paths(&s));

        // The refs are fetched while the user is already on the target step, so
        // wait for the listing rather than for the step to change.
        pump_save_until_refs(&mut save, &mut app);
        let flow = save.flow.as_mut().unwrap();
        assert!(
            flow.refs().branches.iter().any(|b| b == "main"),
            "the branch listing arrived so an existing branch can be appended to"
        );
        flow.target_name = "main".to_string();
        assert!(flow.submit_target());
        assert_eq!(save.step(), SaveStep::CommitMessage);

        save.flow.as_mut().unwrap().message = "from the GUI".to_string();
        start_save(&mut save, &mut app);
        assert!(
            pump_save_until_past(&mut save, &mut app, SaveStep::CommitMessage),
            "the push completed"
        );

        // The commit really landed: read it back out of the bare repo.
        let bare = base.join("bare.git");
        let out = std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "show",
                "main:demo.hurl",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "demo.hurl is on main: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("https://example.com/a"),
            "the request itself was committed, not an empty file"
        );

        let col = &app.session.collections[0];
        assert_eq!(
            col.git_origin.as_ref().map(|o| o.path.as_str()),
            Some("demo.hurl"),
            "the collection remembers where it now lives"
        );
        assert!(
            col.entries.iter().all(|e| !e.user_added && !e.modified),
            "the push counts as this tab's save, so the edit markers are cleared"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
