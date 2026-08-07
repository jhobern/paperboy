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

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::git_remote::{self, GitOrigin, RefKind};
use crate::i18n::{Status, Strings};
use crate::remote_flow::{
    FlowEvent, RefChoice, RemoteFlow, RemoteKind, Step, WorkspaceGitFilter, WorkspaceGitOrigin,
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

#[derive(Clone, Copy)]
enum SaveTarget {
    Collection(usize),
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
    fn visible_files(&self) -> Vec<String> {
        let filter = self.filter.to_lowercase();
        self.flow
            .all_files()
            .iter()
            .filter(|path| self.show_all_files || is_default_load_file(path))
            .filter(|path| filter.is_empty() || path.to_lowercase().contains(&filter))
            .cloned()
            .collect()
    }

    /// The repo files the current [`WorkspaceGitFilter`] would download.
    fn workspace_matches(&self) -> Vec<String> {
        self.flow
            .all_files()
            .iter()
            .filter(|p| self.ws_filter.matches(p))
            .cloned()
            .collect()
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

struct SaveFlow {
    target: SaveTarget,
    initialized: bool,
    /// True when the target collection/environment no longer exists, so saving
    /// is impossible; disables the Save button without matching on error text.
    blocked: bool,
    item_name: String,
    url: String,
    token: String,
    branch: String,
    path: String,
    message: String,
    error: Option<String>,
    rx: Option<Receiver<WorkerMsg>>,
    busy_label: Option<&'static str>,
}

impl SaveFlow {
    fn new(target: SaveTarget) -> Self {
        Self {
            target,
            initialized: false,
            blocked: false,
            item_name: String::new(),
            url: String::new(),
            token: String::new(),
            branch: "main".to_string(),
            path: String::new(),
            message: String::new(),
            error: None,
            rx: None,
            busy_label: None,
        }
    }

    fn ensure_initialized(&mut self, app: &GuiApp) {
        if self.initialized {
            return;
        }
        self.initialized = true;

        match self.target {
            SaveTarget::Collection(ci) => {
                let Some(col) = app.session.collections.get(ci) else {
                    self.error = Some(app.strings.gui_git_err_collection_missing.to_string());
                    self.blocked = true;
                    return;
                };
                self.item_name = col.name.clone();
                self.message = format!("{} {}", app.strings.gui_git_update_prefix, col.name);
                if let Some(origin) = &col.git_origin {
                    self.url = origin.repo_url.clone();
                    self.branch = origin.ref_name.clone();
                    self.path = origin.path.clone();
                } else {
                    self.path = default_save_path(&col.name, col.path.as_deref(), "hurl");
                }
            }
        }
    }

    fn is_busy(&self) -> bool {
        self.rx.is_some()
    }

    fn token_opt(&self) -> Option<String> {
        nonblank(self.token.trim())
    }

    fn poll(&mut self, app: &mut GuiApp) -> bool {
        let Some(msg) = poll_worker(
            &mut self.rx,
            &mut self.busy_label,
            &mut self.error,
            &app.strings,
        ) else {
            return false;
        };

        match msg {
            WorkerMsg::Save(Ok(_)) => self.finish_save(app),
            WorkerMsg::Save(Err(e)) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn finish_save(&mut self, app: &mut GuiApp) -> bool {
        let origin = GitOrigin {
            repo_url: self.url.trim().to_string(),
            path: self.path.trim().to_string(),
            ref_kind: RefKind::Branch,
            ref_name: self.branch.trim().to_string(),
        };

        match self.target {
            SaveTarget::Collection(ci) => {
                let Some(col) = app.session.collections.get_mut(ci) else {
                    self.error = Some(app.strings.gui_git_err_collection_closed.to_string());
                    return false;
                };
                col.git_origin = Some(origin);
                // The push is the collection's save — clear its edit markers
                // exactly as a local Save does.
                col.mark_saved();
            }
        }

        remember_git_url(&mut app.session, &self.url);
        app.session.status = Some(Status::GitSaved);
        app.session.save();
        true
    }
}

/// A finished background push. Loading has no equivalent here: its workers
/// belong to [`crate::remote_flow`], which both front-ends share.
enum WorkerMsg {
    Save(Result<String, String>),
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
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(460.0);
            action = draw_flow(ui, &mut flow, colors, strings);
        });

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
        UiAction::BackToConnect => {
            if let Flow::Load(load) = &mut flow {
                load.flow.back_to_connect();
                load.local_error = None;
            }
        }
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
            if let Flow::Load(load) = &mut flow
                && save_workspace_permanently(load, app)
                && load.finish_workspace(app)
            {
                return;
            }
        }
        UiAction::Save => {
            if let Flow::Save(save) = &mut flow {
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
            Flow::Save(_) => s.gui_git_save_collection_title,
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

    let matched = load.workspace_matches().len();
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
                    for path in visible {
                        let selected = load.selected_path.as_deref() == Some(path.as_str());
                        if super::widgets::selectable(ui, selected, path.as_str()).clicked() {
                            load.selected_path = Some(path);
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
    let mut action = UiAction::None;

    draw_busy_and_error(ui, save.busy_label, save.error.as_deref(), colors);

    ui.colored_label(colors.accent, s.gui_git_repo_url);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut save.url).desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.colored_label(colors.accent, s.gui_git_token);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut save.token).desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.colored_label(colors.accent, s.gui_git_branch);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut save.branch).desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.colored_label(colors.accent, s.gui_git_path);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut save.path).desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);
    ui.colored_label(colors.accent, s.gui_git_commit_message);
    ui.add_enabled(
        !busy,
        egui::TextEdit::singleline(&mut save.message).desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy
                    && !save.url.trim().is_empty()
                    && !save.branch.trim().is_empty()
                    && !save.path.trim().is_empty()
                    && !save.blocked,
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

    action
}

fn poll_worker(
    rx: &mut Option<Receiver<WorkerMsg>>,
    busy_label: &mut Option<&'static str>,
    error: &mut Option<String>,
    s: &Strings,
) -> Option<WorkerMsg> {
    let result = rx.as_ref().map(Receiver::try_recv)?;
    match result {
        Ok(msg) => {
            *rx = None;
            *busy_label = None;
            Some(msg)
        }
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            *rx = None;
            *busy_label = None;
            *error = Some(s.gui_git_err_worker_ended.to_string());
            None
        }
    }
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

fn start_save(save: &mut SaveFlow, app: &mut GuiApp) {
    let Some(url) = nonblank(save.url.trim()) else {
        save.error = Some(app.strings.gui_git_err_url_required.to_string());
        return;
    };
    let Some(branch) = nonblank(save.branch.trim()) else {
        save.error = Some(app.strings.gui_git_err_branch_required.to_string());
        return;
    };
    let path = match clean_repo_path(&save.path, &app.strings) {
        Ok(path) => path,
        Err(e) => {
            save.error = Some(e);
            return;
        }
    };
    let message = nonblank(save.message.trim()).unwrap_or_else(|| {
        let item = if save.item_name.trim().is_empty() {
            app.strings.gui_untitled
        } else {
            save.item_name.trim()
        };
        format!("{} {item}", app.strings.gui_git_update_prefix)
    });

    let content = match save.target {
        SaveTarget::Collection(ci) => {
            let Some(col) = app.session.collections.get(ci) else {
                save.error = Some(app.strings.gui_git_err_collection_missing.to_string());
                return;
            };
            col.to_hurl()
        }
    };

    save.url = url.clone();
    save.branch = branch.clone();
    save.path = path.clone();
    save.message = message.clone();
    save.error = None;
    save.rx = Some(spawn_save(
        url,
        save.token_opt(),
        branch,
        path,
        content,
        message,
    ));
    save.busy_label = Some(app.strings.gui_git_saving);
}

fn start_workspace_checkout(load: &mut LoadFlow, s: &Strings) {
    if load.workspace_matches().is_empty() {
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
fn save_workspace_permanently(load: &mut LoadFlow, app: &mut GuiApp) -> bool {
    let Some(source) = load.ws_root.clone() else {
        load.local_error = Some(app.strings.gui_git_err_browse_again.to_string());
        return false;
    };
    let Some(name) = nonblank(&load.ws_name) else {
        load.local_error = Some(app.strings.gui_git_err_ws_name_required.to_string());
        return false;
    };
    let Some(parent) = super::filepick::pick_folder(
        app.strings.git_workspace_storage_choose,
        app.session.last_browse_dir.as_deref(),
    ) else {
        return false; // cancelled — stay on the question, keep it temporary
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

fn spawn_save(
    url: String,
    token: Option<String>,
    branch: String,
    path: String,
    content: String,
    message: String,
) -> Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // The three remote-git calls are dependent, so they live in one worker
        // and the GUI only polls a single result while the window stays alive.
        let author = git_remote::author_identity();
        let target_ref = git_remote::branch_ref(&branch);
        let result = match git_remote::fetch_base(&url, token.as_deref(), &target_ref) {
            Ok((repo, base_sha)) => {
                let pushed = (|| {
                    let new_sha = git_remote::commit_files(
                        &repo,
                        &base_sha,
                        &[(path, content)],
                        &message,
                        &author.0,
                        &author.1,
                    )?;
                    git_remote::push_commit(&url, token.as_deref(), &repo, &new_sha, &target_ref)?;
                    Ok(new_sha)
                })();
                git_remote::scrub_remote(&repo);
                git_remote::cleanup(&repo);
                pushed
            }
            Err(e) => Err(e),
        };
        let _ = tx.send(WorkerMsg::Save(result));
    });
    rx
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

fn default_save_path(name: &str, local_path: Option<&Path>, ext: &str) -> String {
    if let Some(file_name) = local_path
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
    {
        return file_name.to_string();
    }

    let mut file_name = name
        .trim()
        .chars()
        .map(|ch| if matches!(ch, '/' | '\\') { '_' } else { ch })
        .collect::<String>();
    if file_name.is_empty() {
        file_name = "paperboy".to_string();
    }
    if Path::new(&file_name).extension().is_none() {
        file_name.push('.');
        file_name.push_str(ext);
    }
    file_name
}

fn clean_repo_path(path: &str, s: &Strings) -> Result<String, String> {
    let mut cleaned = path.trim().replace('\\', "/");
    while let Some(rest) = cleaned.strip_prefix("./") {
        cleaned = rest.to_string();
    }
    if cleaned.is_empty() {
        return Err(s.gui_git_err_path_required.to_string());
    }
    for component in Path::new(&cleaned).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(s.gui_git_err_path_relative.to_string());
            }
        }
    }
    Ok(cleaned)
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

    #[test]
    fn the_file_filter_is_case_insensitive() {
        let mut load = seeded(LoadTarget::File, &["api/Health.hurl", "api/orders.hurl"]);
        load.filter = "HEALTH".to_string();
        assert_eq!(load.visible_files(), vec!["api/Health.hurl"]);
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
        assert_eq!(load.workspace_matches().len(), 2, "the .bin is left behind");
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
}
