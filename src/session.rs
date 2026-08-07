//! Front-end-agnostic application session.
//!
//! This is the single home for the *core state* (open tabs/collections, the
//! global environments, the shared response buffer, user settings and themes)
//! and the *orchestration logic* (running requests, resolving the effective
//! environment, loading collections/environments, tab management and
//! persistence) that used to live only inside the terminal UI's `TuiApp`.
//!
//! Both front-ends drive the exact same logic through this module so there is
//! no duplication: the GUI owns a [`Session`] directly, and the terminal UI's
//! pure/duplicated helpers delegate to the free functions here (see
//! [`effective_env`], [`shadowed_env_keys`], [`active_theme_spec`]).

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use crate::collection::Collection;
use crate::environment::{
    EnvUpdate, Environment, PendingEnvSecrets, looks_like_env, parse_vars_pending,
    spawn_resolution, spawn_resolution_many,
};
use crate::git_remote::GitOrigin;
use crate::http::ApiResponse;
use crate::hurl::RunStatus;
use crate::i18n::{Language, Status};
use crate::persistence::{
    self, GuiLayout, PendingWorkspaceReload, PersistedEnv, PersistedReport, PersistedState,
    PersistedTab,
};
use crate::request::{self, AppVars, BatchRunUpdate, CaptureUpdate, RequestView};
use crate::theme::{self, ThemeSpec};
use crate::tui::remote::WorkspaceGitOrigin;

// ── Shared pure helpers (called by both front-ends) ─────────────────────────

/// Build the effective, merged [`Environment`] used for substitution in
/// collection `ci`: the active Global Environment's vars, overridden by the
/// collection's own Linked Environment's vars on any name collision (Linked
/// wins). `None` when neither is set.
pub fn effective_env(
    collections: &[Collection],
    global_envs: &[Environment],
    ci: usize,
    active_env_id: Option<u64>,
) -> Option<Environment> {
    let linked = collections
        .get(ci)
        .and_then(|c| c.linked_env_id)
        .and_then(|id| global_envs.iter().find(|e| e.id == id));
    let active = active_env_id.and_then(|id| global_envs.iter().find(|e| e.id == id));
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

/// Keys defined in *both* the active collection's linked Environment and the
/// active Global Environment — per [`effective_env`]'s merge rule the linked
/// value always wins, so these keys' Global Environment value is silently
/// shadowed. Used to flag such substitutions with a warning icon.
pub fn shadowed_env_keys(
    collections: &[Collection],
    global_envs: &[Environment],
    ci: usize,
    active_env_id: Option<u64>,
) -> HashSet<String> {
    let linked = collections
        .get(ci)
        .and_then(|c| c.linked_env_id)
        .and_then(|id| global_envs.iter().find(|e| e.id == id));
    let active = active_env_id.and_then(|id| global_envs.iter().find(|e| e.id == id));
    match (linked, active) {
        (Some(linked), Some(active)) if linked.id != active.id => linked
            .vars
            .iter()
            .filter(|lv| active.vars.iter().any(|av| av.key == lv.key))
            .map(|lv| lv.key.clone())
            .collect(),
        _ => HashSet::new(),
    }
}

/// Every selectable theme, in display order: the built-in presets followed by
/// the user's custom themes.
pub fn all_themes(custom_themes: &[ThemeSpec]) -> Vec<ThemeSpec> {
    let mut themes = theme::builtin_presets();
    themes.extend(custom_themes.iter().cloned());
    themes
}

/// Look a theme up by name across presets and custom themes.
pub fn find_theme(name: &str, custom_themes: &[ThemeSpec]) -> Option<ThemeSpec> {
    all_themes(custom_themes)
        .into_iter()
        .find(|t| t.name == name)
}

/// The theme spec currently in effect: the manually-chosen theme if set (and
/// still present), otherwise the current language's preset.
pub fn active_theme_spec(
    active_theme: Option<&str>,
    custom_themes: &[ThemeSpec],
    language: &Language,
) -> ThemeSpec {
    if let Some(name) = active_theme
        && let Some(spec) = find_theme(name, custom_themes)
    {
        return spec;
    }
    theme::preset_for_language(language)
}

/// Which "last used" directory a file picker should start from. Environments
/// get their own memory because they usually live somewhere quite different
/// from collections (a shared secrets folder vs. a project tree), so a single
/// shared directory would send one picker to the other's folder every time.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub enum PickerKind {
    Environment,
    Other,
}

// ── The Session ─────────────────────────────────────────────────────────────

/// The whole front-end-agnostic application state. The GUI holds one of these;
/// the terminal UI keeps its own view state but shares this module's logic.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub struct Session {
    pub language: Language,
    pub vars: AppVars,

    /// Open collection tabs. `collections[0]` is the built-in "Request" tab.
    pub collections: Vec<Collection>,
    /// The active tab index (into `collections`).
    pub active_tab: usize,

    /// The global environments, shared across every collection.
    pub global_envs: Vec<Environment>,
    /// The activated Global Environment id, if any.
    pub active_env_id: Option<u64>,

    /// The shared response buffer written by the background request runner.
    pub response: Arc<Mutex<ApiResponse>>,

    /// In-flight background work (drained by [`Session::poll`]).
    pub pending_env: Vec<Receiver<EnvUpdate>>,
    pub pending_captures: Vec<Receiver<CaptureUpdate>>,
    pub pending_batch_runs: Vec<Receiver<BatchRunUpdate>>,

    /// User-created themes and the explicitly-chosen theme name (`None` follows
    /// the language preset).
    pub custom_themes: Vec<ThemeSpec>,
    pub active_theme: Option<String>,

    // Persisted settings / preferences.
    pub confirm_on_exit: bool,
    pub confirm_on_clear: bool,
    pub confirm_on_delete_env: bool,
    pub always_save_when_prompted: bool,
    pub default_request_view: RequestView,
    pub run_all_batch_mode: bool,
    pub list_width: u16,
    pub response_pct: u16,
    pub recent_git_urls: Vec<String>,
    pub last_browse_dir: Option<PathBuf>,
    pub last_env_dir: Option<PathBuf>,
    /// Window/panel geometry and last-open view for the graphical front-end.
    /// The terminal UI never reads it but still round-trips it, so alternating
    /// between the two front-ends doesn't wipe the GUI's layout.
    pub gui: GuiLayout,

    /// A transient status message for the footer.
    pub status: Option<Status>,

    /// Persisted report tabs, preserved verbatim so a session saved from one
    /// front-end never drops the reports the other front-end created. The GUI's
    /// reports panel manages these through [`Session::reports`] accessors.
    pub reports: Vec<PersistedReport>,
    /// Workspace tabs restored with a vanished `workspace_root` that are known
    /// to have been downloaded from git, paired with the tab index they were
    /// restored at. Filled by [`Session::apply_persisted`]; the front-end drains
    /// this to offer redownloading each one (see
    /// [`crate::persistence::PendingWorkspaceReload`]) rather than silently
    /// resetting the tab. Transient — never persisted.
    pub pending_workspace_reloads: VecDeque<(usize, PendingWorkspaceReload)>,
    /// The active report index within a workspace tab, mirrored so persistence
    /// round-trips faithfully. Front-ends own their own richer view state.
    active_report: Option<usize>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            language: Language::default(),
            vars: AppVars::default(),
            collections: vec![Collection::new("Request".to_string(), Vec::new())],
            active_tab: 0,
            global_envs: Vec::new(),
            active_env_id: None,
            response: Arc::new(Mutex::new(ApiResponse::default())),
            pending_env: Vec::new(),
            pending_captures: Vec::new(),
            pending_batch_runs: Vec::new(),
            custom_themes: Vec::new(),
            active_theme: None,
            confirm_on_exit: true,
            confirm_on_clear: true,
            confirm_on_delete_env: true,
            always_save_when_prompted: false,
            default_request_view: RequestView::default(),
            run_all_batch_mode: false,
            list_width: 38,
            response_pct: 42,
            recent_git_urls: Vec::new(),
            last_browse_dir: None,
            last_env_dir: None,
            gui: GuiLayout::default(),
            status: None,
            reports: Vec::new(),
            pending_workspace_reloads: VecDeque::new(),
            active_report: None,
        }
    }
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
impl Session {
    /// A session restored from the persisted `state.json`, or a fresh one when
    /// there is nothing to restore.
    pub fn restored() -> Self {
        let mut s = Self::default();
        if let Some(state) = persistence::load_state() {
            s.apply_persisted(state);
        }
        s
    }

    // ── Theme ────────────────────────────────────────────────────────────

    pub fn all_themes(&self) -> Vec<ThemeSpec> {
        all_themes(&self.custom_themes)
    }

    pub fn active_theme_spec(&self) -> ThemeSpec {
        active_theme_spec(
            self.active_theme.as_deref(),
            &self.custom_themes,
            &self.language,
        )
    }

    // ── Environments ──────────────────────────────────────────────────────

    /// The merged environment used for substitution in collection `ci`.
    pub fn effective_env(&self, ci: usize) -> Option<Environment> {
        effective_env(&self.collections, &self.global_envs, ci, self.active_env_id)
    }

    /// Keys whose active Global Environment value is silently shadowed by the
    /// collection's linked Environment (see the free [`shadowed_env_keys`]).
    pub fn shadowed_env_keys(&self, ci: usize) -> HashSet<String> {
        shadowed_env_keys(&self.collections, &self.global_envs, ci, self.active_env_id)
    }

    /// Toggle which Global Environment is active (activating the same one again
    /// deactivates it), rebuilding affected previews.
    pub fn set_active_env(&mut self, env_id: Option<u64>) {
        self.active_env_id = if self.active_env_id == env_id {
            None
        } else {
            env_id
        };
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        self.save();
    }

    /// Link (pin) a Global Environment to collection `ci` (linking the same one
    /// again unlinks it).
    pub fn set_linked_env(&mut self, ci: usize, env_id: Option<u64>) {
        if let Some(col) = self.collections.get_mut(ci) {
            col.linked_env_id = if col.linked_env_id == env_id {
                None
            } else {
                env_id
            };
            col.invalidate_request_json();
        }
        self.save();
    }

    /// Load a `.vars` environment from text. Returns its id on success. On a
    /// name collision with an existing environment the new one is renamed with
    /// a numeric suffix (the GUI resolves collisions with an explicit dialog if
    /// it prefers).
    pub fn load_environment_text(
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
        // Disambiguate a duplicate name so both stay usable.
        if self.global_envs.iter().any(|e| e.name == env.name) {
            let base = env.name.clone();
            let mut n = 2;
            while self
                .global_envs
                .iter()
                .any(|e| e.name == format!("{base} ({n})"))
            {
                n += 1;
            }
            env.name = format!("{base} ({n})");
        }
        env.path = path;
        env.git_origin = git_origin;
        let id = env.id;
        self.global_envs.push(env);
        for col in &mut self.collections {
            col.invalidate_request_json();
        }
        if !pending.is_empty() {
            self.pending_env.push(spawn_resolution(id, pending));
        }
        self.status = Some(Status::Loaded);
        self.save();
        Some(id)
    }

    /// Delete the Global Environment with `env_id`, unlinking any collections
    /// that referenced it.
    pub fn delete_environment(&mut self, env_id: u64) {
        self.global_envs.retain(|e| e.id != env_id);
        if self.active_env_id == Some(env_id) {
            self.active_env_id = None;
        }
        for col in &mut self.collections {
            if col.linked_env_id == Some(env_id) {
                col.linked_env_id = None;
            }
            col.invalidate_request_json();
        }
        self.save();
    }

    // ── Collections / tabs ────────────────────────────────────────────────

    /// Load a collection (Hurl or Postman JSON) from text into a new tab.
    /// Returns `true` on success.
    pub fn load_collection_text(
        &mut self,
        name: String,
        content: &str,
        path: Option<PathBuf>,
    ) -> bool {
        let entries = crate::postman::parse_collection(content);
        if entries.is_empty() {
            let reason = if crate::postman::looks_like_postman(content) {
                None
            } else {
                crate::hurl::parse_hurl_error(content)
            };
            self.status = Some(match reason {
                Some(why) => Status::Error(format!("{why}")),
                None => Status::NotCollection,
            });
            return false;
        }
        let mut col = Collection::new(name, entries);
        col.path = path;
        self.collections.push(col);
        self.active_tab = self.collections.len() - 1;
        self.status = Some(Status::Loaded);
        self.save();
        true
    }

    /// Append a fresh empty collection tab and make it active.
    pub fn add_collection(&mut self, name: impl Into<String>) -> usize {
        self.collections
            .push(Collection::new(name.into(), Vec::new()));
        let idx = self.collections.len() - 1;
        self.active_tab = idx;
        idx
    }

    /// Append a fresh empty Global Environment and return its id.
    pub fn add_environment(&mut self, name: impl Into<String>) -> u64 {
        let env = Environment {
            id: crate::environment::next_env_id(),
            name: name.into(),
            vars: Vec::new(),
            path: None,
            git_origin: None,
        };
        let id = env.id;
        self.global_envs.push(env);
        self.save();
        id
    }

    /// Total number of collection tabs.
    pub fn tab_count(&self) -> usize {
        self.collections.len()
    }

    pub fn activate_tab(&mut self, idx: usize) {
        if idx < self.tab_count() {
            self.active_tab = idx;
        }
    }

    /// The directory a file picker for `kind` should open at: the last folder
    /// the user picked something of that kind from, falling back to the general
    /// last-browsed folder so a first-ever environment picker still lands
    /// somewhere useful rather than the process's working directory.
    pub fn picker_dir(&self, kind: PickerKind) -> Option<&std::path::Path> {
        let specific = match kind {
            PickerKind::Environment => self.last_env_dir.as_deref(),
            PickerKind::Other => None,
        };
        specific
            .or(self.last_browse_dir.as_deref())
            .filter(|d| d.is_dir())
    }

    /// Remember where a picker just landed, so the next one reopens there.
    /// `path` may be the chosen file itself — its parent directory is stored.
    pub fn remember_picker_dir(&mut self, kind: PickerKind, path: &std::path::Path) {
        let dir = if path.is_dir() {
            Some(path.to_path_buf())
        } else {
            path.parent().map(|p| p.to_path_buf())
        };
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            return;
        };
        if kind == PickerKind::Environment {
            self.last_env_dir = Some(dir.clone());
        }
        self.last_browse_dir = Some(dir);
    }

    /// Close tab `idx` (the built-in Request tab at index 0 is never closed).
    pub fn close_tab(&mut self, idx: usize) {
        self.close_tab_inner(idx, false);
    }

    /// Close tab `idx` *and* wipe its Workspace folder from disk. Only ever
    /// valid for a tab whose folder the app downloaded itself
    /// ([`Collection::workspace_downloaded_from_git`]) and only when the user
    /// explicitly asked for it — a folder the user picked from their own
    /// filesystem is never deleted, so this silently falls back to an ordinary
    /// close for any other tab.
    pub fn close_tab_deleting_workspace(&mut self, idx: usize) {
        self.close_tab_inner(idx, true);
    }

    fn close_tab_inner(&mut self, idx: usize, delete_workspace: bool) {
        if idx == 0 || idx >= self.collections.len() {
            return;
        }
        let removed = self.collections.remove(idx);
        if delete_workspace
            && removed.workspace_downloaded_from_git
            && let Some(root) = &removed.workspace_root
        {
            crate::git_remote::cleanup(root);
        }
        if self.active_tab >= self.collections.len() {
            self.active_tab = self.collections.len() - 1;
        }
        self.save();
    }

    // ── Workspaces ────────────────────────────────────────────────────────

    /// Open a folder as a Workspace: a new tab whose file tree is the real
    /// filesystem under `root` (see [`Collection::ws_rows`]). The tab starts
    /// with no loaded collection; selecting a `.hurl`/`.json` file in the tree
    /// loads it. Returns the new tab index.
    pub fn open_workspace(&mut self, root: PathBuf) -> usize {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        self.push_workspace_tab(name, root, None)
    }

    /// Open a Workspace whose (filtered) files were just downloaded from git
    /// into `root` — a throwaway directory reused in place as the tab's
    /// `workspace_root`, exactly like a locally picked folder, since its
    /// checked-out files already sit at the right relative paths. `name` comes
    /// from the repo URL rather than the meaningless temp-directory name, and
    /// `origin` records the exact commit so the download can be repeated if
    /// the folder later vanishes (e.g. the OS clears `/tmp`).
    ///
    /// Unlike a single-file load this directory is deliberately *not* cleaned
    /// up afterwards — the tab reads from it live for as long as it stays open.
    /// `workspace_downloaded_from_git` marks it as the app's own throwaway, so
    /// closing the tab may offer to delete it (a folder the user picked
    /// themselves must never be deleted).
    pub fn open_workspace_from_git(
        &mut self,
        root: PathBuf,
        name: String,
        origin: Option<WorkspaceGitOrigin>,
    ) -> usize {
        self.push_workspace_tab(name, root, origin)
    }

    /// Rebind Workspace tab `idx` to `root`, a folder that has just been
    /// redownloaded from git for a tab whose original download had vanished
    /// (see [`Session::pending_workspace_reloads`]). The file that was open
    /// last session is re-selected if it still exists in the new download —
    /// its path has to be re-resolved *relatively*, because the fresh checkout
    /// lands in a different temp directory than the one recorded.
    pub fn rebind_redownloaded_workspace(
        &mut self,
        idx: usize,
        root: PathBuf,
        relative_selected_path: Option<String>,
    ) {
        let Some(col) = self.collections.get_mut(idx) else {
            return;
        };
        let selected = relative_selected_path
            .map(|rel| root.join(rel))
            .filter(|p| p.exists());
        col.workspace_root = Some(root);
        col.workspace_downloaded_from_git = true;
        // The redownload may not contain the file that was open last time (the
        // filter or the commit's contents can differ), so a failed reopen just
        // leaves the tab on its tree rather than being an error.
        match selected {
            Some(path) => {
                if col.load_workspace_file(path).is_err() {
                    col.path = None;
                }
            }
            None => col.path = None,
        }
        self.status = Some(Status::WorkspaceReloaded);
        self.save();
    }

    fn push_workspace_tab(
        &mut self,
        name: String,
        root: PathBuf,
        git_origin: Option<WorkspaceGitOrigin>,
    ) -> usize {
        let mut col = Collection::new(name, Vec::new());
        col.workspace_root = Some(root);
        col.workspace_downloaded_from_git = git_origin.is_some();
        col.workspace_git_origin = git_origin;
        self.collections.push(col);
        let ci = self.collections.len() - 1;
        self.active_tab = ci;
        self.save();
        ci
    }

    /// Load a collection file from a Workspace tab's tree into that tab (the
    /// shared core of the terminal UI's workspace file open). Returns `true` on
    /// success; on an I/O error it sets an error status and returns `false`.
    pub fn load_workspace_file(&mut self, ci: usize, path: PathBuf) -> bool {
        let Some(col) = self.collections.get_mut(ci) else {
            return false;
        };
        match col.load_workspace_file(path) {
            Ok(()) => {
                self.active_tab = ci;
                self.status = Some(Status::Loaded);
                self.save();
                true
            }
            Err(e) => {
                self.status = Some(Status::Error(e.to_string()));
                false
            }
        }
    }

    /// Load a `.vars` environment file selected from a Workspace tree as a
    /// Global Environment (the same path as File → Load → Environment). Returns
    /// the new environment's id on success.
    pub fn open_workspace_environment(&mut self, path: &std::path::Path) -> Option<u64> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                self.status = Some(Status::Error(e.to_string()));
                return None;
            }
        };
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("env")
            .to_string();
        self.load_environment_text(name, &content, Some(path.to_path_buf()), None)
    }

    // ── Running requests ──────────────────────────────────────────────────

    fn begin_request(&self) {
        self.response.lock().unwrap().begin();
    }

    /// Run the selected request of collection `ci`. Returns the blocking secret
    /// keys (and does nothing) if the request references still-loading secrets.
    pub fn run_entry(&mut self, ci: usize) -> Vec<String> {
        if self.collections.get(ci).is_none() {
            return Vec::new();
        }
        let env = self.effective_env(ci);
        let blocking = request::pending_request_keys(&self.collections[ci], env.as_ref());
        if !blocking.is_empty() {
            self.status = Some(Status::WaitingSecrets(blocking.clone()));
            return blocking;
        }
        self.status = None;
        self.begin_request();
        let selected = self.collections[ci].selected_entry;
        if let Some(entry) = self.collections[ci].entries.get_mut(selected) {
            entry.last_run = RunStatus::Running;
        }
        if let Some(rx) =
            request::run_collection(&self.collections[ci], env.as_ref(), self.response.clone())
        {
            self.pending_captures.push(rx);
        }
        Vec::new()
    }

    /// Run every request of collection `ci` in order ("Run All").
    pub fn run_all_entries(&mut self, ci: usize) -> Vec<String> {
        let Some(col) = self.collections.get(ci) else {
            return Vec::new();
        };
        if col.entries.is_empty() {
            return Vec::new();
        }
        let env = self.effective_env(ci);
        let blocking = request::pending_request_keys_all(col, env.as_ref());
        if !blocking.is_empty() {
            self.status = Some(Status::WaitingSecrets(blocking.clone()));
            return blocking;
        }
        self.status = None;
        self.begin_request();
        for entry in self.collections[ci].entries.iter_mut() {
            entry.last_run = RunStatus::Running;
        }
        if let Some(rx) = request::run_all_entries(
            &self.collections[ci],
            env.as_ref(),
            self.response.clone(),
            self.run_all_batch_mode,
        ) {
            self.pending_batch_runs.push(rx);
        }
        Vec::new()
    }

    /// Drain every in-flight background result (secret resolution, single-run
    /// captures and "Run All" passes) and apply them. Returns `true` if any
    /// state changed (so a front-end can request a repaint).
    pub fn poll(&mut self) -> bool {
        let before_env = self.pending_env.len();
        let before_cap = self.pending_captures.len();
        let before_batch = self.pending_batch_runs.len();

        request::drain_env_updates(
            &mut self.pending_env,
            &mut self.global_envs,
            &mut self.collections,
        );
        request::drain_capture_updates(&mut self.pending_captures, &mut self.collections);
        self.drain_batch_runs();

        // A crude but effective "something happened" signal: any queue shrank,
        // or a response is still loading (spinner needs animating).
        before_env != self.pending_env.len()
            || before_cap != self.pending_captures.len()
            || before_batch != self.pending_batch_runs.len()
            || self.response.lock().map(|r| r.loading).unwrap_or(false)
    }

    fn drain_batch_runs(&mut self) {
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
                                    None => RunStatus::Running,
                                };
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

    // ── Persistence ───────────────────────────────────────────────────────

    /// Snapshot for saving (environments in source form only — resolved secrets
    /// are never written to disk).
    pub fn to_persisted(&self) -> PersistedState {
        PersistedState {
            language: self.language.clone(),
            base_url: self.vars.base_url.clone(),
            tabs: self
                .collections
                .iter()
                .map(|c| {
                    let linked_env_index = c
                        .linked_env_id
                        .and_then(|id| self.global_envs.iter().position(|e| e.id == id));
                    PersistedTab::from_collection(c, linked_env_index)
                })
                .collect(),
            reports: self.reports.clone(),
            active_tab: self.active_tab,
            last_browse_dir: self
                .last_browse_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            last_env_dir: self
                .last_env_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            confirm_on_exit: self.confirm_on_exit,
            confirm_on_clear: self.confirm_on_clear,
            confirm_on_delete_env: self.confirm_on_delete_env,
            always_save_when_prompted: self.always_save_when_prompted,
            list_width: self.list_width,
            response_pct: self.response_pct,
            recent_git_urls: self.recent_git_urls.clone(),
            default_request_view: self.default_request_view,
            run_all_batch_mode: self.run_all_batch_mode,
            custom_themes: self.custom_themes.clone(),
            active_theme: self.active_theme.clone(),
            global_envs: self
                .global_envs
                .iter()
                .map(PersistedEnv::from_environment)
                .collect(),
            active_global_env: self
                .active_env_id
                .and_then(|id| self.global_envs.iter().position(|e| e.id == id)),
            gui: self.gui,
        }
    }

    /// Restore from persisted state.
    pub fn apply_persisted(&mut self, state: PersistedState) {
        self.language = state.language;
        if !state.base_url.trim().is_empty() {
            self.vars.base_url = state.base_url;
        }

        let mut pending_groups = Vec::new();
        let mut global_envs = Vec::with_capacity(state.global_envs.len());
        for pe in &state.global_envs {
            let (env, pending) = pe.restore();
            if !pending.is_empty() {
                pending_groups.push(PendingEnvSecrets {
                    env_id: env.id,
                    pending,
                });
            }
            global_envs.push(env);
        }
        self.active_env_id = state
            .active_global_env
            .and_then(|idx| global_envs.get(idx))
            .map(|e| e.id);
        self.global_envs = global_envs;
        if !pending_groups.is_empty() {
            self.pending_env.push(spawn_resolution_many(pending_groups));
        }

        if !state.tabs.is_empty() {
            let mut collections = Vec::with_capacity(state.tabs.len());
            let mut reloads = VecDeque::new();
            for (idx, tab) in state.tabs.into_iter().enumerate() {
                let linked_env_id = tab
                    .linked_env_index
                    .and_then(|i| self.global_envs.get(i))
                    .map(|e| e.id);
                let (col, pending_reload) = tab.into_collection(linked_env_id);
                // A git-downloaded Workspace whose folder has vanished since
                // the last session (typically `/tmp` swept between restarts)
                // is queued rather than silently reset — the front-end offers
                // to redownload it, pinned to the exact commit it recorded.
                if let Some(reload) = pending_reload {
                    reloads.push_back((idx, reload));
                }
                collections.push(col);
            }
            self.collections = collections;
            self.pending_workspace_reloads = reloads;
        }

        self.reports = state.reports;
        self.active_report = None;

        self.active_tab = state.active_tab.min(self.tab_count().saturating_sub(1));
        self.last_browse_dir = state
            .last_browse_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        self.last_env_dir = state
            .last_env_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        self.gui = state.gui;
        self.confirm_on_exit = state.confirm_on_exit;
        self.confirm_on_clear = state.confirm_on_clear;
        self.confirm_on_delete_env = state.confirm_on_delete_env;
        self.always_save_when_prompted = state.always_save_when_prompted;
        self.list_width = state.list_width;
        self.response_pct = state.response_pct;
        self.recent_git_urls = state.recent_git_urls;
        self.default_request_view = state.default_request_view;
        self.run_all_batch_mode = state.run_all_batch_mode;
        self.custom_themes = state.custom_themes;
        self.active_theme = state.active_theme;
    }

    /// Persist the current state to disk.
    pub fn save(&self) {
        persistence::save_state(&self.to_persisted());
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    use crate::collection::WsRow;
    use crate::tui::remote::WorkspaceGitFilter;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_ws_session_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("api")).unwrap();
        std::fs::write(dir.join("health.hurl"), "GET https://example.com/health\n").unwrap();
        std::fs::write(
            dir.join("api/users.hurl"),
            "GET https://example.com/users\nHTTP 200\n\nGET https://example.com/users/1\n",
        )
        .unwrap();
        std::fs::write(dir.join("api/dev.vars"), "BASE_URL=https://example.com\n").unwrap();
        std::fs::write(dir.join("api/run.trail"), "{\"nodes\":[]}\n").unwrap();
        dir
    }

    #[test]
    fn open_workspace_tab_lists_the_folder_tree_and_loads_files_and_envs() {
        let dir = tmp("open");
        let mut s = Session::default();
        let ci = s.open_workspace(dir.clone());

        // A Workspace tab is a collection with a root and no loaded entries.
        assert!(s.collections[ci].is_workspace());
        assert!(s.collections[ci].entries.is_empty());

        // The top level lists the `api` folder and `health.hurl` (dirs first).
        let rows = s.collections[ci].ws_rows();
        assert!(matches!(&rows[0], WsRow::Folder { name, .. } if name == "api"));
        assert!(
            rows.iter()
                .any(|r| matches!(r, WsRow::Collection { name, .. } if name == "health.hurl"))
        );

        // Expand `api` so its children (a collection, an env and a report) show.
        let api = dir.join("api");
        s.collections[ci].workspace_expanded.insert(api.clone());
        let rows = s.collections[ci].ws_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, WsRow::Collection { name, .. } if name == "users.hurl"))
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r, WsRow::Environment { name, .. } if name == "dev.vars"))
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r, WsRow::Report { name, .. } if name == "run.trail"))
        );

        // Loading a collection file brings its requests into the tab in place.
        assert!(s.load_workspace_file(ci, api.join("users.hurl")));
        assert_eq!(
            s.collections[ci].path.as_deref(),
            Some(api.join("users.hurl").as_path())
        );
        assert_eq!(s.collections[ci].entries.len(), 2);

        // The loaded file's requests now appear as detailed rows under it.
        let rows = s.collections[ci].ws_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, WsRow::Request { loaded: true, .. }))
        );

        // Selecting a `.vars` file loads it as a global environment.
        let before = s.global_envs.len();
        assert!(
            s.open_workspace_environment(&api.join("dev.vars"))
                .is_some()
        );
        assert_eq!(s.global_envs.len(), before + 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exit warning has to count a Workspace tab's *parked* edits — a file
    /// the user edited and then switched away from is precisely the case with
    /// nothing on disk to fall back on — without double-counting the file the
    /// tab is currently showing.
    #[test]
    fn unsaved_edits_are_counted_once_across_parked_and_loaded_workspace_files() {
        let dir = tmp("count");
        let mut s = Session::default();
        s.collections.clear();
        let ci = s.open_workspace(dir.clone());

        assert!(s.load_workspace_file(ci, dir.join("api/users.hurl")));
        s.collections[ci].entries[0].modified = true;
        s.collections[ci].entries[1].modified = true;
        assert_eq!(
            s.collections[ci].unsaved_edit_count(),
            2,
            "the loaded file's own edits"
        );

        // Switching away parks those two and loads a clean file.
        assert!(s.load_workspace_file(ci, dir.join("health.hurl")));
        assert_eq!(
            s.collections[ci].unsaved_edit_count(),
            2,
            "parked edits still count, and the clean loaded file adds none"
        );

        // Editing the new file too adds to the total rather than replacing it.
        s.collections[ci].entries[0].modified = true;
        assert_eq!(s.collections[ci].unsaved_edit_count(), 3);

        // Coming back must not count the same edits twice: the file is now both
        // loaded and still listed in `workspace_pending`.
        assert!(s.load_workspace_file(ci, dir.join("api/users.hurl")));
        assert_eq!(
            s.collections[ci].unsaved_edit_count(),
            3,
            "a file that is both loaded and parked is counted once"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quitting only warns about edits a quit would really destroy.
    ///
    /// A plain tab's requests are written to the session state exactly as they
    /// stand, edit markers and all, so they come back edited next start — the
    /// warning used to count those and so appeared on every single quit, and
    /// went on appearing no matter how many times it was dismissed, because
    /// nothing about the edits ever changed.
    #[test]
    fn quitting_only_counts_edits_that_a_restart_would_not_bring_back() {
        let dir = tmp("lost");
        let mut s = Session::default();
        s.collections.clear();

        // A plain tab, edited and never saved to its file.
        let mut plain = crate::collection::Collection::new(
            "plain".to_string(),
            vec![crate::hurl::HurlEntry::default()],
        );
        plain.path = Some(dir.join("plain.hurl"));
        plain.entries[0].modified = true;
        s.collections.push(plain);

        // And a Workspace tab, likewise.
        let ci = s.open_workspace(dir.clone());
        assert!(s.load_workspace_file(ci, dir.join("api/users.hurl")));
        s.collections[ci].entries[0].modified = true;

        assert_eq!(
            s.collections[0].unsaved_edit_count(),
            1,
            "closing the plain tab would still throw its edit away"
        );
        assert_eq!(
            s.collections[0].edits_lost_on_exit(),
            0,
            "but quitting would not: the session state keeps it, still flagged"
        );
        assert_eq!(
            s.collections[ci].edits_lost_on_exit(),
            1,
            "while a Workspace tab is re-read from disk on restore, so its edit \
             really would be gone"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The GUI's pixel geometry survives a save/load round-trip. (The matching
    /// terminal-UI half — that it carries the field through untouched — is
    /// asserted in `tui::tests`, where `TuiApp` is reachable.)
    #[test]
    fn the_gui_layout_round_trips_through_a_save_and_load() {
        let layout = GuiLayout {
            window: Some((1440.0, 900.0)),
            left_width: Some(312.0),
            env_height: Some(240.0),
            response_height: Some(360.0),
            report_diag_height: Some(96.0),
            report_palette_width: Some(200.0),
            view: crate::persistence::GuiView::Report(2),
            report_source_view: true,
        };

        let mut s = Session::default();
        s.gui = layout;
        let saved = s.to_persisted();
        let mut restored = Session::default();
        restored.apply_persisted(saved);
        assert_eq!(restored.gui, layout, "the GUI restores its own layout");
    }

    #[test]
    fn load_workspace_file_on_a_bad_index_or_path_fails_without_panicking() {
        let dir = tmp("bad");
        let mut s = Session::default();
        let ci = s.open_workspace(dir.clone());

        // A non-existent file path is reported as a failure, not a panic.
        assert!(!s.load_workspace_file(ci, dir.join("nope.hurl")));
        // An out-of-range collection index is a graceful false.
        assert!(!s.load_workspace_file(999, dir.join("health.hurl")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edits_survive_switching_to_another_collection_and_back() {
        let dir = tmp("pending");
        let mut s = Session::default();
        let ci = s.open_workspace(dir.clone());

        // Edit the first request of health.hurl, exactly as the request editor
        // does (write through to the entry, then flag it).
        assert!(s.load_workspace_file(ci, dir.join("health.hurl")));
        s.collections[ci].entries[0].url = "https://example.com/health?deep=1".into();
        s.collections[ci].entries[0].modified = true;
        assert!(s.collections[ci].workspace_file_edited(&dir.join("health.hurl")));

        // Look at a different collection in the same Workspace tab...
        assert!(s.load_workspace_file(ci, dir.join("api/users.hurl")));
        assert_eq!(s.collections[ci].entries.len(), 2, "users.hurl is loaded");
        // ...the edit is still remembered against the file it belongs to,
        // even though those entries are no longer the tab's live ones.
        assert!(
            s.collections[ci].workspace_file_edited(&dir.join("health.hurl")),
            "switching away must not discard unsaved edits"
        );
        // ...and the individual request still reads as edited, so the tree can
        // pencil the row even while a different collection is the loaded one.
        assert!(
            s.collections[ci].workspace_request_edited(&dir.join("health.hurl"), 0),
            "a parked request is still an edited request"
        );
        assert!(
            !s.collections[ci].workspace_request_edited(&dir.join("api/users.hurl"), 0),
            "an untouched request in the loaded file carries no pencil"
        );

        // ...and coming back hands them straight back rather than re-reading
        // the (unchanged) file from disk.
        assert!(s.load_workspace_file(ci, dir.join("health.hurl")));
        assert_eq!(
            s.collections[ci].entries[0].url,
            "https://example.com/health?deep=1"
        );
        assert!(s.collections[ci].entries[0].modified);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_collection_clears_its_edit_markers_and_parked_edits() {
        let dir = tmp("saved");
        let mut s = Session::default();
        let ci = s.open_workspace(dir.clone());

        assert!(s.load_workspace_file(ci, dir.join("health.hurl")));
        s.collections[ci].entries[0].modified = true;
        s.collections[ci].mark_saved();
        assert!(
            !s.collections[ci].workspace_file_edited(&dir.join("health.hurl")),
            "a saved file matches disk, so it carries no pencil"
        );

        // A file saved while parked is likewise no longer pending, so
        // reopening it reads the (now current) file rather than stale entries.
        s.collections[ci].entries[0].modified = true;
        assert!(s.load_workspace_file(ci, dir.join("api/users.hurl")));
        assert!(
            s.collections[ci]
                .workspace_pending
                .contains_key(&dir.join("health.hurl"))
        );
        assert!(s.load_workspace_file(ci, dir.join("health.hurl")));
        s.collections[ci].mark_saved();
        assert!(s.collections[ci].workspace_pending.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn git_origin(url: &str) -> WorkspaceGitOrigin {
        WorkspaceGitOrigin {
            repo_url: url.to_string(),
            commit_sha: "abc123".into(),
            ref_kind: crate::git_remote::RefKind::Branch,
            ref_name: "main".into(),
            filter: WorkspaceGitFilter::All,
        }
    }

    #[test]
    fn a_workspace_opened_from_git_records_where_it_came_from() {
        let dir = tmp("fromgit");
        let mut s = Session::default();
        let origin = git_origin("https://example.com/repo.git");
        let ci = s.open_workspace_from_git(dir.clone(), "repo".into(), Some(origin.clone()));

        assert!(s.collections[ci].is_workspace());
        assert!(s.collections[ci].workspace_downloaded_from_git);
        assert_eq!(
            s.collections[ci]
                .workspace_git_origin
                .as_ref()
                .map(|o| &o.repo_url),
            Some(&origin.repo_url)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_downloaded_workspace_can_delete_its_folder_but_a_local_one_never_is() {
        let downloaded = tmp("del_git");
        let local = tmp("del_local");
        let mut s = Session::default();

        let ci = s.open_workspace_from_git(
            downloaded.clone(),
            "repo".into(),
            Some(git_origin("https://example.com/repo.git")),
        );
        s.close_tab_deleting_workspace(ci);
        assert!(!downloaded.exists(), "a git download is ours to delete");

        // The same call on a folder the user chose themselves must leave it
        // alone — deleting it would destroy files PaperBoy never created.
        let ci = s.open_workspace(local.clone());
        s.close_tab_deleting_workspace(ci);
        assert!(local.exists(), "a user's own folder is never deleted");

        let _ = std::fs::remove_dir_all(&local);
    }

    #[test]
    fn a_redownloaded_workspace_reselects_the_file_that_was_open_before() {
        let dir = tmp("rebind");
        let mut s = Session::default();
        let ci = s.open_workspace_from_git(
            dir.clone(),
            "repo".into(),
            Some(git_origin("https://example.com/repo.git")),
        );

        s.rebind_redownloaded_workspace(ci, dir.clone(), Some("api/users.hurl".into()));

        assert_eq!(
            s.collections[ci].workspace_root.as_deref(),
            Some(dir.as_path())
        );
        assert!(s.collections[ci].workspace_downloaded_from_git);
        assert_eq!(
            s.collections[ci].path.as_deref(),
            Some(dir.join("api/users.hurl").as_path())
        );
        assert!(matches!(
            s.status,
            Some(crate::i18n::Status::WorkspaceReloaded)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
