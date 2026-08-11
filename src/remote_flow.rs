//! The "load something from a git remote" wizard, shared by both front-ends.
//!
//! [`crate::git_remote`] holds the git operations themselves; this module holds
//! the *flow* around them — which step comes next, what is fetched when, what
//! is kept and what is thrown away — so the terminal UI and the GUI cannot
//! disagree about any of it. Each front-end supplies only presentation: how a
//! list is drawn and which key or click advances it.
//!
//! The two front-ends previously each had their own copy of this state machine
//! and had already drifted apart in ways users could see (the GUI recorded
//! recently-used URLs but never showed them; it could not load a report at all;
//! it offered branches and tags as two lists behind a toggle where the terminal
//! UI offered one filterable list). Sharing the flow is what stops that
//! recurring.
//!
//! The flow is deliberately not `async`: each step spawns one thread and hands
//! back a [`Receiver`], which the terminal UI polls on its 120ms tick and the
//! GUI polls each frame. That suits two front-ends with completely different
//! event loops better than either owning the other's scheduler.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::git_remote::{self, GitOrigin, RefKind, RemoteRefs};
use crate::i18n::Strings;

/// What the wizard is loading: one collection, one environment, one PaperTrail
/// `.trail`, or a whole workspace (every matching file at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKind {
    Collection,
    Environment,
    Report,
    Workspace,
}

impl RemoteKind {
    /// Whether this kind picks a single file, or a batch by file type.
    pub(crate) fn is_workspace(self) -> bool {
        matches!(self, RemoteKind::Workspace)
    }
}

/// Which files to download when loading a whole workspace, asked right after
/// the branch/tag is chosen — a big repo may hold plenty of large, unrelated
/// files that should never be pulled down just to browse its collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum WorkspaceGitFilter {
    HurlAndJson,
    HurlOnly,
    JsonOnly,
    All,
}

impl WorkspaceGitFilter {
    pub(crate) const ALL: [WorkspaceGitFilter; 4] = [
        WorkspaceGitFilter::HurlAndJson,
        WorkspaceGitFilter::HurlOnly,
        WorkspaceGitFilter::JsonOnly,
        WorkspaceGitFilter::All,
    ];

    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            WorkspaceGitFilter::HurlAndJson => s.git_ws_filter_hurl_json,
            WorkspaceGitFilter::HurlOnly => s.git_ws_filter_hurl,
            WorkspaceGitFilter::JsonOnly => s.git_ws_filter_json,
            WorkspaceGitFilter::All => s.git_ws_filter_all,
        }
    }

    /// Whether `path` (a repo-relative path as returned by `git ls-tree`)
    /// should be downloaded under this filter (case-insensitive extension
    /// match, mirroring `crate::workspace::scan_workspace`'s local filter).
    pub(crate) fn matches(self, path: &str) -> bool {
        let ext = Path::new(path).extension().and_then(|e| e.to_str());
        match self {
            WorkspaceGitFilter::All => true,
            WorkspaceGitFilter::HurlAndJson => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl") || e.eq_ignore_ascii_case("json"))
            }
            WorkspaceGitFilter::HurlOnly => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl"))
            }
            WorkspaceGitFilter::JsonOnly => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("json"))
            }
        }
    }
}

/// Where a workspace's downloaded files came from in git — remembered on
/// [`crate::collection::Collection::workspace_git_origin`] so that if the temp
/// download folder vanishes (e.g. the OS clears `/tmp` between sessions), the
/// app can offer to redownload the exact same commit rather than losing track
/// of the workspace entirely.
///
/// Pinned to a commit **sha**, not just a branch/tag name, so a redownload
/// restores exactly what was there before (not "whatever the branch points at
/// now") — and a failed fetch of that exact sha is a reliable sign the remote's
/// history no longer contains it (force-push, rebase, deleted branch/tag),
/// which is reported to the user as such. Never holds a token (tokens are
/// intentionally never persisted, same as [`crate::git_remote::GitOrigin`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkspaceGitOrigin {
    pub(crate) repo_url: String,
    pub(crate) commit_sha: String,
    pub(crate) ref_kind: RefKind,
    pub(crate) ref_name: String,
    pub(crate) filter: WorkspaceGitFilter,
}

/// A branch or tag choice: `label` is shown to the user, `gitref` is the full
/// ref (e.g. `refs/heads/main`) passed to `git fetch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefChoice {
    pub(crate) label: String,
    pub(crate) gitref: String,
}

// ---------------------------------------------------------------------------
// Temp repo ownership
// ---------------------------------------------------------------------------

/// A temp repo created by [`git_remote::list_files`], cleaned up when dropped.
///
/// Cleanup is tied to ownership rather than to any particular exit path
/// because there are many: the app can be closed, a worker's result can be
/// abandoned in a channel nobody reads, or the user can cancel at any of five
/// steps. Anything less reliable leaves token-bearing git remotes on disk.
///
/// A workspace load is the one case that *keeps* the folder — the checkout
/// becomes the tab's live workspace root — so it takes ownership away with
/// [`TempRepo::keep`], which disarms the cleanup.
#[derive(Debug)]
pub(crate) struct TempRepo {
    path: Option<PathBuf>,
}

impl TempRepo {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub(crate) fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("TempRepo used after its folder was given away")
    }

    /// Take the folder out of the handle: the caller owns it from here and it
    /// will **not** be scrubbed or deleted on drop.
    pub(crate) fn keep(mut self) -> PathBuf {
        self.path
            .take()
            .expect("TempRepo used after its folder was given away")
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            // Drop the `origin` remote before deleting, so a token in
            // `.git/config` doesn't survive in any backup of the temp dir.
            git_remote::scrub_remote(path);
            git_remote::cleanup(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Steps and messages
// ---------------------------------------------------------------------------

/// Which step of the wizard is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Entering the repo URL and (optionally) an access token.
    Connect,
    /// Choosing a branch or tag.
    PickRef,
    /// Choosing one file.
    PickFile,
    /// Workspace only: choosing which file types to download, before anything
    /// is checked out.
    PickWorkspaceFilter,
    /// Workspace only: the files are downloaded — keep them in the temp folder,
    /// or copy them somewhere permanent now?
    WorkspaceStorage,
}

/// The background git operation in flight, for the "please wait" message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Refs,
    Files,
    File,
    WorkspaceFiles,
}

impl Phase {
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            Phase::Refs => s.git_loading_refs,
            Phase::Files => s.git_loading_files,
            Phase::File => s.git_loading_file,
            Phase::WorkspaceFiles => s.git_loading_workspace_files,
        }
    }
}

/// A completed background git operation.
enum FlowMsg {
    Refs(Result<RemoteRefs, String>),
    /// `(files, repo, commit_sha)` — the sha is the exact commit the listing
    /// was fetched at, remembered so a workspace load can later be redownloaded
    /// pinned to it rather than to "whatever the branch points at now".
    Files(Result<(Vec<String>, TempRepo, String), String>),
    Content(Result<String, String>),
    Workspace(Result<(), String>),
}

/// Something the front-end must act on. Everything else — advancing a step,
/// recording an error, cleaning up a temp folder — the flow handles itself.
#[derive(Debug)]
pub(crate) enum FlowEvent {
    /// One file was fetched. The front-end turns it into a collection,
    /// environment or report tab according to [`RemoteFlow::kind`].
    Content {
        path: String,
        text: String,
        origin: Option<GitOrigin>,
    },
    /// A workspace's files were downloaded into `root`, which the front-end now
    /// owns (it will not be cleaned up).
    Workspace {
        root: PathBuf,
        name: String,
        origin: Option<WorkspaceGitOrigin>,
    },
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

/// The state machine behind "load from a git remote".
///
/// The front-end owns presentation (selection indices, filter text, editors);
/// everything that decides *what happens next* lives here.
pub(crate) struct RemoteFlow {
    pub(crate) kind: RemoteKind,
    pub(crate) url: String,
    pub(crate) token: String,
    step: Step,
    /// The branches and tags from the last `list_refs`, kept raw: they are
    /// formatted for display at draw time, when the front-end has its
    /// [`Strings`] to hand.
    refs: RemoteRefs,
    /// The full file listing from `list_files`, kept whole so the workspace
    /// filter step can reuse it without a second network fetch.
    files: Vec<String>,
    /// Which listing `files` currently holds, taken from a process-wide counter
    /// whenever it is replaced. A front-end that derives something from the
    /// listing (the GUI's filtered picker) can key a cache on this and know
    /// exactly when the answer it is holding stopped being true — length and
    /// commit both compare equal across two different repos often enough to be
    /// no answer at all. The counter is global rather than per-flow so a whole
    /// flow being replaced by another one is a change too.
    files_generation: u64,
    repo: Option<TempRepo>,
    commit_sha: Option<String>,
    chosen_ref: Option<RefChoice>,
    chosen_path: Option<String>,
    chosen_ws_filter: Option<WorkspaceGitFilter>,
    error: Option<String>,
    busy: Option<Phase>,
    rx: Option<Receiver<FlowMsg>>,
}

impl RemoteFlow {
    pub(crate) fn new(kind: RemoteKind) -> Self {
        RemoteFlow {
            kind,
            url: String::new(),
            token: String::new(),
            step: Step::Connect,
            refs: RemoteRefs::default(),
            files: Vec::new(),
            files_generation: 0,
            repo: None,
            commit_sha: None,
            chosen_ref: None,
            chosen_path: None,
            chosen_ws_filter: None,
            error: None,
            busy: None,
            rx: None,
        }
    }

    // -- inspection -------------------------------------------------------

    pub(crate) fn step(&self) -> Step {
        self.step
    }

    pub(crate) fn busy(&self) -> Option<Phase> {
        self.busy
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn clear_error(&mut self) {
        self.error = None;
    }

    /// Record a failure the front-end detected itself, rather than one that
    /// came back from git — an empty URL, or a filter that matches nothing.
    /// It shows exactly like any other error, on the step the user is on.
    pub(crate) fn fail(&mut self, message: String) {
        self.busy = None;
        self.rx = None;
        self.error = Some(message);
    }

    /// The branches and tags exactly as the remote reported them, for a
    /// front-end that presents the two separately rather than as one list.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn refs(&self) -> &RemoteRefs {
        &self.refs
    }

    /// Whether the fetched checkout is still around. It is dropped as soon as
    /// it has served its purpose, so a front-end offering "load this file" can
    /// tell the user the repo needs browsing again rather than doing nothing.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn has_repo(&self) -> bool {
        self.repo.is_some()
    }

    /// Whether a background operation is in flight.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// The commit the current file listing was fetched at, shown so the user
    /// can see exactly what a download will be pinned to.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }

    /// The branch/tag choices to offer, as one filterable list.
    pub(crate) fn ref_choices(&self, s: &Strings) -> Vec<RefChoice> {
        build_ref_choices(&self.refs, s)
    }

    /// The paths worth offering for this kind — see [`relevant_files`].
    pub(crate) fn pickable_files(&self) -> Vec<String> {
        relevant_files(self.kind, &self.files)
    }

    /// Every path fetched, regardless of kind. Offered behind a "show all"
    /// affordance for a repo that names its files unusually.
    pub(crate) fn all_files(&self) -> &[String] {
        &self.files
    }

    /// Which listing [`Self::all_files`] is currently serving — see
    /// [`Self::files_generation`].
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn files_generation(&self) -> u64 {
        self.files_generation
    }

    fn set_files(&mut self, files: Vec<String>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.files = files;
        self.files_generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn token_opt(&self) -> Option<String> {
        let t = self.token.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }

    // -- transitions ------------------------------------------------------

    /// Start: fetch the repo's branches and tags.
    pub(crate) fn connect(&mut self) {
        if self.url.trim().is_empty() {
            return;
        }
        self.error = None;
        self.busy = Some(Phase::Refs);
        self.rx = Some(spawn_refs(self.url.trim().to_string(), self.token_opt()));
    }

    /// A branch or tag was chosen: list that ref's files.
    pub(crate) fn choose_ref(&mut self, choice: RefChoice) {
        self.error = None;
        self.busy = Some(Phase::Files);
        self.chosen_ref = Some(choice.clone());
        self.rx = Some(spawn_files(
            self.url.trim().to_string(),
            self.token_opt(),
            choice.gitref,
        ));
    }

    /// A file was chosen: fetch its contents.
    pub(crate) fn choose_file(&mut self, path: String) {
        let Some(repo) = &self.repo else {
            return;
        };
        self.error = None;
        self.busy = Some(Phase::File);
        self.chosen_path = Some(path.clone());
        self.rx = Some(spawn_checkout(repo.path().to_path_buf(), path));
    }

    /// A file-type filter was chosen: download every matching file.
    pub(crate) fn choose_workspace_filter(&mut self, filter: WorkspaceGitFilter) {
        let Some(repo) = &self.repo else {
            return;
        };
        let paths: Vec<String> = self
            .files
            .iter()
            .filter(|p| filter.matches(p))
            .cloned()
            .collect();
        self.error = None;
        self.busy = Some(Phase::WorkspaceFiles);
        self.chosen_ws_filter = Some(filter);
        self.rx = Some(spawn_workspace_checkout(repo.path().to_path_buf(), paths));
    }

    /// Go back to the URL step, discarding anything fetched. Used by "back"
    /// affordances in both front-ends.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn back_to_connect(&mut self) {
        self.repo = None;
        self.files.clear();
        self.refs = RemoteRefs::default();
        self.commit_sha = None;
        self.chosen_ref = None;
        self.error = None;
        self.busy = None;
        self.rx = None;
        self.step = Step::Connect;
    }

    /// Go back to the branch/tag step, discarding the fetched checkout but
    /// keeping the refs — so picking a different branch doesn't mean listing
    /// the repo's refs all over again.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn back_to_refs(&mut self) {
        self.repo = None;
        self.files.clear();
        self.commit_sha = None;
        self.chosen_path = None;
        self.error = None;
        self.busy = None;
        self.rx = None;
        self.step = Step::PickRef;
    }

    // -- polling ----------------------------------------------------------

    /// Take one completed background result, if any, and advance.
    ///
    /// Returns the event the front-end must act on, or `None` when the flow
    /// handled the result itself (a step change or an error). Call this from
    /// the front-end's own loop; it never blocks.
    pub(crate) fn poll(&mut self) -> Option<FlowEvent> {
        let msg = match self.rx.as_ref()?.try_recv() {
            Ok(msg) => msg,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                // The worker died without sending — don't leave the UI showing
                // a spinner that will never stop.
                self.rx = None;
                self.busy = None;
                return None;
            }
        };
        self.rx = None;
        self.apply(msg)
    }

    /// Advance on one completed result. Split from [`RemoteFlow::poll`] so
    /// every transition can be tested without spawning a thread or reaching a
    /// network.
    fn apply(&mut self, msg: FlowMsg) -> Option<FlowEvent> {
        self.busy = None;
        match msg {
            FlowMsg::Refs(Ok(refs)) => {
                self.refs = refs;
                self.step = Step::PickRef;
                None
            }
            FlowMsg::Files(Ok((files, repo, sha))) => {
                self.repo = Some(repo);
                self.set_files(files);
                self.commit_sha = Some(sha);
                self.step = if self.kind.is_workspace() {
                    Step::PickWorkspaceFilter
                } else {
                    Step::PickFile
                };
                None
            }
            FlowMsg::Content(Ok(text)) => {
                // Whether the file parses or not, this fetched repo has served
                // its purpose; dropping it scrubs and removes the folder.
                self.repo = None;
                Some(FlowEvent::Content {
                    path: self.chosen_path.clone().unwrap_or_default(),
                    text,
                    origin: self.git_origin(),
                })
            }
            FlowMsg::Workspace(Ok(())) => {
                let origin = self.workspace_origin();
                // The checkout succeeded, so the folder is the workspace's live
                // content rather than a scratch clone — take it out of the
                // handle so it survives the wizard closing.
                let root = self.repo.take()?.keep();
                self.step = Step::WorkspaceStorage;
                Some(FlowEvent::Workspace {
                    root,
                    name: workspace_name_from_url(&self.url),
                    origin,
                })
            }
            FlowMsg::Refs(Err(e)) => {
                self.error = Some(e);
                self.step = Step::Connect;
                None
            }
            FlowMsg::Files(Err(e)) => {
                self.error = Some(e);
                self.step = Step::PickRef;
                None
            }
            FlowMsg::Content(Err(e)) => {
                self.error = Some(e);
                self.step = Step::PickFile;
                None
            }
            FlowMsg::Workspace(Err(e)) => {
                self.error = Some(e);
                self.step = Step::PickWorkspaceFilter;
                None
            }
        }
    }

    // -- provenance -------------------------------------------------------

    /// The [`GitOrigin`] for the file just checked out, so a later "save to
    /// git" can write back where it came from. `None` if either piece is
    /// missing, which shouldn't happen — both are set before any checkout.
    fn git_origin(&self) -> Option<GitOrigin> {
        let choice = self.chosen_ref.as_ref()?;
        let path = self.chosen_path.as_ref()?;
        let (ref_kind, ref_name) = git_remote::parse_ref_kind(&choice.gitref);
        Some(GitOrigin {
            repo_url: self.url.trim().to_string(),
            ref_kind,
            ref_name,
            path: path.clone(),
        })
    }

    /// The [`WorkspaceGitOrigin`] for the batch just downloaded, pinned to the
    /// exact commit it came from.
    fn workspace_origin(&self) -> Option<WorkspaceGitOrigin> {
        let choice = self.chosen_ref.as_ref()?;
        let sha = self.commit_sha.as_ref()?;
        let filter = self.chosen_ws_filter?;
        let (ref_kind, ref_name) = git_remote::parse_ref_kind(&choice.gitref);
        Some(WorkspaceGitOrigin {
            repo_url: self.url.trim().to_string(),
            commit_sha: sha.clone(),
            ref_kind,
            ref_name,
            filter,
        })
    }
}

/// Test-only seeding, so a front-end's tests can put a flow on the step they
/// care about without spawning threads or reaching a network. Driving the real
/// transitions is [`RemoteFlow`]'s own job and is tested here.
#[cfg(test)]
impl RemoteFlow {
    pub(crate) fn seed(
        kind: RemoteKind,
        url: &str,
        step: Step,
        files: Vec<String>,
        repo: Option<PathBuf>,
    ) -> Self {
        let mut flow = RemoteFlow::new(kind);
        flow.url = url.to_string();
        flow.step = step;
        flow.set_files(files);
        flow.repo = repo.map(TempRepo::new);
        flow
    }

    /// Pretend a ref listing arrived, for tests that start at the ref picker.
    /// Only the GUI's tests need it; the terminal UI's use `seed_refs_from`.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn seed_refs(&mut self, branches: &[&str], tags: &[&str]) {
        self.refs = RemoteRefs {
            branches: branches.iter().map(|b| b.to_string()).collect(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        };
        self.step = Step::PickRef;
    }

    /// Pretend a background operation is in flight, for drawing tests.
    pub(crate) fn seed_busy(&mut self, phase: Phase) {
        self.busy = Some(phase);
    }

    /// Pretend a branch was chosen, for provenance assertions.
    pub(crate) fn seed_ref(&mut self, gitref: &str, sha: &str) {
        self.chosen_ref = Some(RefChoice {
            label: gitref.to_string(),
            gitref: gitref.to_string(),
        });
        self.commit_sha = Some(sha.to_string());
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the flat, filterable branch+tag choice list (branches first).
///
/// One list rather than two behind a toggle: a repo's refs are easier to search
/// than to categorise, and the user usually knows the name they want. Each is
/// still prefixed with its kind, because a branch and a tag can share a name
/// and fetching the wrong one is confusing to diagnose.
pub(crate) fn build_ref_choices(refs: &RemoteRefs, s: &Strings) -> Vec<RefChoice> {
    let mut out = Vec::with_capacity(refs.branches.len() + refs.tags.len());
    for b in &refs.branches {
        out.push(RefChoice {
            label: format!("[{}] {b}", s.git_branches),
            gitref: git_remote::branch_ref(b),
        });
    }
    for t in &refs.tags {
        out.push(RefChoice {
            label: format!("[{}] {t}", s.git_tags),
            gitref: git_remote::tag_ref(t),
        });
    }
    out
}

/// Narrow a git file listing to the paths worth showing in a single-file
/// picker, so loading from a big repo isn't buried under unrelated files:
///   * a collection load shows only `.hurl` / `.json` files;
///   * an environment load shows `.vars` / `.env` files (including `.env`-style
///     dotfiles like `.env` and `.env.dev-au`) and `.json` (Postman exports an
///     environment as JSON);
///   * a report load shows only `.trail` files.
///
/// If nothing matches (an unusually named repo), the full list is returned
/// unchanged rather than leaving the user staring at an empty picker.
pub(crate) fn relevant_files(kind: RemoteKind, files: &[String]) -> Vec<String> {
    let keep = |path: &String| -> bool {
        let p = Path::new(path);
        let ext = p.extension().and_then(|e| e.to_str());
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        match kind {
            RemoteKind::Collection => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("hurl") || e.eq_ignore_ascii_case("json"))
            }
            RemoteKind::Environment => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("vars")
                    || e.eq_ignore_ascii_case("env")
                    || e.eq_ignore_ascii_case("json"))
                    || name.eq_ignore_ascii_case(".env")
                    || name.to_ascii_lowercase().starts_with(".env.")
            }
            RemoteKind::Report => {
                matches!(ext, Some(e) if e.eq_ignore_ascii_case("trail"))
            }
            // A workspace load uses the file-type filter step, not this picker.
            RemoteKind::Workspace => true,
        }
    };
    let filtered: Vec<String> = files.iter().filter(|p| keep(p)).cloned().collect();
    if filtered.is_empty() {
        files.to_vec()
    } else {
        filtered
    }
}

/// Indices of `items` whose text contains `filter` (case-insensitive).
pub(crate) fn filter_indices<'a>(items: impl Iterator<Item = &'a str>, filter: &str) -> Vec<usize> {
    let f = filter.to_lowercase();
    items
        .enumerate()
        .filter(|(_, s)| f.is_empty() || s.to_lowercase().contains(&f))
        .map(|(i, _)| i)
        .collect()
}

/// A tab name for a workspace loaded from `url`, taken from the repo name.
pub(crate) fn workspace_name_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let stem = last.strip_suffix(".git").unwrap_or(last);
    if stem.is_empty() {
        "workspace".to_string()
    } else {
        stem.to_string()
    }
}

// ---------------------------------------------------------------------------
// Workers
// ---------------------------------------------------------------------------

fn spawn_refs(url: String, token: Option<String>) -> Receiver<FlowMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(FlowMsg::Refs(git_remote::list_refs(&url, token.as_deref())));
    });
    rx
}

fn spawn_files(url: String, token: Option<String>, gitref: String) -> Receiver<FlowMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = git_remote::list_files(&url, token.as_deref(), &gitref)
            .map(|(files, repo, sha)| (files, TempRepo::new(repo), sha));
        // If the wizard was cancelled the receiver is gone; the returned
        // `SendError` carries the `TempRepo`, so dropping it here cleans the
        // folder up rather than leaking a token-bearing checkout.
        let _ = tx.send(FlowMsg::Files(result));
    });
    rx
}

fn spawn_checkout(repo: PathBuf, path: String) -> Receiver<FlowMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(FlowMsg::Content(git_remote::checkout_file(&repo, &path)));
    });
    rx
}

/// Check out a workspace's filtered batch of `paths` into `repo`, which the
/// flow keeps as the new tab's workspace root. The folder outlives the wizard
/// on success, so its `origin` remote is dropped first — otherwise the access
/// token used to fetch it would sit in the kept folder's `.git/config`.
fn spawn_workspace_checkout(repo: PathBuf, paths: Vec<String>) -> Receiver<FlowMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = git_remote::checkout_files(&repo, &paths);
        if result.is_ok() {
            git_remote::scrub_remote(&repo);
        }
        let _ = tx.send(FlowMsg::Workspace(result));
    });
    rx
}

/// Redownload a workspace's files, pinned to the exact `commit_sha` recorded
/// when it was first downloaded — used when the local temp folder has vanished
/// (e.g. `/tmp` was cleared) since the last session.
///
/// Unlike the interactive flow this never prompts for a branch/tag or a
/// file-type filter (both are already known) and never asks for a token
/// (tokens are never persisted), so a private repo fails here with an auth
/// error; the recourse is to load it again through the wizard, which does ask.
/// A failure to fetch the exact recorded sha most often means the remote's
/// history no longer contains it (force-push, rebase, or a deleted branch/tag).
pub(crate) fn spawn_workspace_redownload(
    origin: WorkspaceGitOrigin,
) -> Receiver<Result<PathBuf, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = match git_remote::list_files(&origin.repo_url, None, &origin.commit_sha) {
            Ok((files, repo, _sha)) => {
                let matched: Vec<String> = files
                    .into_iter()
                    .filter(|p| origin.filter.matches(p))
                    .collect();
                match git_remote::checkout_files(&repo, &matched) {
                    Ok(()) => {
                        git_remote::scrub_remote(&repo);
                        Ok(repo)
                    }
                    Err(e) => {
                        git_remote::cleanup(&repo);
                        Err(e)
                    }
                }
            }
            Err(e) => Err(e),
        };
        // If the caller gave up waiting (app closed mid-fetch), clean up any
        // downloaded folder rather than leaking it.
        if let Err(mpsc::SendError(Ok(dir))) = tx.send(result) {
            git_remote::cleanup(&dir);
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    /// A workspace downloaded from git records the *commit* it came from, not
    /// just the branch name, so a later "reload" fetches the same bytes even if
    /// the branch has moved on since.
    #[test]
    fn a_downloaded_workspace_remembers_the_exact_commit() {
        let mut flow = super::RemoteFlow::new(super::RemoteKind::Workspace);
        flow.url = "  https://example.test/repo.git  ".to_string();
        flow.chosen_ws_filter = Some(super::WorkspaceGitFilter::All);
        flow.seed_ref("refs/heads/main", "0123456789abcdef");

        let origin = flow.workspace_origin().expect("both pieces were set");
        assert_eq!(origin.commit_sha, "0123456789abcdef");
        assert_eq!(origin.ref_name, "main");
        assert_eq!(
            origin.repo_url, "https://example.test/repo.git",
            "the url is stored trimmed, so it matches on a later comparison"
        );
    }

    use super::*;

    fn refs_of(branches: &[&str], tags: &[&str]) -> RemoteRefs {
        RemoteRefs {
            branches: branches.iter().map(|s| s.to_string()).collect(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn branches_and_tags_become_one_list_with_branches_first() {
        let s = Strings::for_language(&crate::i18n::Language::English);
        let choices = build_ref_choices(&refs_of(&["main", "dev"], &["v1.0"]), &s);
        let labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
        // Prefixed, because a branch and a tag may share a name.
        assert_eq!(labels[0], format!("[{}] main", s.git_branches));
        assert_eq!(labels[2], format!("[{}] v1.0", s.git_tags));
        assert_eq!(choices[0].gitref, "refs/heads/main");
        assert_eq!(choices[2].gitref, "refs/tags/v1.0");
    }

    #[test]
    fn a_collection_load_hides_unrelated_files() {
        let files = vec![
            "api.hurl".to_string(),
            "README.md".to_string(),
            "postman.json".to_string(),
            "prod.vars".to_string(),
        ];
        assert_eq!(
            relevant_files(RemoteKind::Collection, &files),
            vec!["api.hurl", "postman.json"]
        );
    }

    #[test]
    fn an_environment_load_finds_dotenv_files() {
        let files = vec![
            ".env".to_string(),
            ".env.dev-au".to_string(),
            "prod.vars".to_string(),
            "api.hurl".to_string(),
        ];
        assert_eq!(
            relevant_files(RemoteKind::Environment, &files),
            vec![".env", ".env.dev-au", "prod.vars"]
        );
    }

    #[test]
    fn a_report_load_shows_only_trail_files() {
        let files = vec!["weekly.trail".to_string(), "api.hurl".to_string()];
        assert_eq!(
            relevant_files(RemoteKind::Report, &files),
            vec!["weekly.trail"]
        );
    }

    #[test]
    fn a_repo_matching_nothing_shows_everything_rather_than_an_empty_list() {
        let files = vec!["requests.txt".to_string(), "notes.md".to_string()];
        assert_eq!(relevant_files(RemoteKind::Collection, &files), files);
    }

    #[test]
    fn the_workspace_filter_matches_by_extension_case_insensitively() {
        assert!(WorkspaceGitFilter::HurlAndJson.matches("a/b/API.HURL"));
        assert!(WorkspaceGitFilter::HurlAndJson.matches("x.json"));
        assert!(!WorkspaceGitFilter::HurlAndJson.matches("x.md"));
        assert!(WorkspaceGitFilter::HurlOnly.matches("x.hurl"));
        assert!(!WorkspaceGitFilter::HurlOnly.matches("x.json"));
        assert!(WorkspaceGitFilter::All.matches("anything.at.all"));
    }

    #[test]
    fn filtering_a_list_is_case_insensitive() {
        let items = ["Main", "release/v2", "dev"];
        assert_eq!(filter_indices(items.iter().copied(), "MAIN"), vec![0]);
        assert_eq!(filter_indices(items.iter().copied(), ""), vec![0, 1, 2]);
        assert_eq!(filter_indices(items.iter().copied(), "e"), vec![1, 2]);
    }

    #[test]
    fn a_workspace_is_named_after_its_repo() {
        assert_eq!(workspace_name_from_url("https://x.com/o/api.git"), "api");
        assert_eq!(workspace_name_from_url("https://x.com/o/api/"), "api");
        assert_eq!(workspace_name_from_url("git@github.com:o/api.git"), "api");
        assert_eq!(workspace_name_from_url(""), "workspace");
    }

    #[test]
    fn a_new_flow_starts_at_the_url_step_with_nothing_in_flight() {
        let flow = RemoteFlow::new(RemoteKind::Collection);
        assert_eq!(flow.step(), Step::Connect);
        assert!(flow.busy().is_none());
        assert!(flow.error().is_none());
    }

    #[test]
    fn connecting_without_a_url_does_nothing() {
        // Otherwise the UI shows a spinner for a request that was never made.
        let mut flow = RemoteFlow::new(RemoteKind::Collection);
        flow.url = "   ".to_string();
        flow.connect();
        assert!(flow.busy().is_none());
    }

    #[test]
    fn a_blank_token_is_treated_as_no_token() {
        let mut flow = RemoteFlow::new(RemoteKind::Collection);
        assert_eq!(flow.token_opt(), None);
        flow.token = "  ".to_string();
        assert_eq!(flow.token_opt(), None);
        flow.token = " ghp_x ".to_string();
        assert_eq!(flow.token_opt(), Some("ghp_x".to_string()));
    }

    #[test]
    fn refs_arriving_advance_to_the_ref_picker() {
        let mut flow = RemoteFlow::new(RemoteKind::Collection);
        flow.apply(FlowMsg::Refs(Ok(refs_of(&["main"], &[]))));
        assert_eq!(flow.step(), Step::PickRef);
        assert_eq!(
            flow.ref_choices(&Strings::for_language(&crate::i18n::Language::English))
                .len(),
            1
        );
    }

    #[test]
    fn a_file_listing_advances_to_the_file_picker_but_a_workspace_to_the_filter() {
        for (kind, expected) in [
            (RemoteKind::Collection, Step::PickFile),
            (RemoteKind::Workspace, Step::PickWorkspaceFilter),
        ] {
            let mut flow = RemoteFlow::new(kind);
            let dir = std::env::temp_dir().join(format!("pb-flow-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            flow.apply(FlowMsg::Files(Ok((
                vec!["a.hurl".to_string()],
                TempRepo::new(dir),
                "abc123".to_string(),
            ))));
            assert_eq!(flow.step(), expected);
        }
    }

    #[test]
    fn each_failure_returns_to_the_step_that_can_fix_it() {
        // An error at any point must leave the user somewhere they can act,
        // not stranded on a dead step.
        let cases: Vec<(RemoteKind, FlowMsg, Step)> = vec![
            (
                RemoteKind::Collection,
                FlowMsg::Refs(Err("no".into())),
                Step::Connect,
            ),
            (
                RemoteKind::Collection,
                FlowMsg::Files(Err("no".into())),
                Step::PickRef,
            ),
            (
                RemoteKind::Collection,
                FlowMsg::Content(Err("no".into())),
                Step::PickFile,
            ),
            (
                RemoteKind::Workspace,
                FlowMsg::Workspace(Err("no".into())),
                Step::PickWorkspaceFilter,
            ),
        ];
        for (kind, msg, expected) in cases {
            let mut flow = RemoteFlow::new(kind);
            flow.apply(msg);
            assert_eq!(flow.step(), expected);
            assert!(flow.error().is_some());
            assert!(flow.busy().is_none(), "a failure must clear the spinner");
        }
    }

    #[test]
    fn going_back_to_the_url_step_discards_the_fetched_repo() {
        let mut flow = RemoteFlow::new(RemoteKind::Collection);
        flow.apply(FlowMsg::Refs(Ok(refs_of(&["main"], &[]))));
        flow.back_to_connect();
        assert_eq!(flow.step(), Step::Connect);
        assert!(
            flow.ref_choices(&Strings::for_language(&crate::i18n::Language::English))
                .is_empty()
        );
        assert!(flow.all_files().is_empty());
    }

    #[test]
    fn provenance_is_recorded_for_a_loaded_file() {
        let mut flow = RemoteFlow::new(RemoteKind::Collection);
        flow.url = "https://git.test/o/r.git".into();
        flow.chosen_ref = Some(RefChoice {
            label: "main".into(),
            gitref: "refs/heads/main".into(),
        });
        flow.chosen_path = Some("api/x.hurl".into());
        let origin = flow.git_origin().expect("origin");
        assert_eq!(origin.repo_url, "https://git.test/o/r.git");
        assert_eq!(origin.ref_kind, RefKind::Branch);
        assert_eq!(origin.ref_name, "main");
        assert_eq!(origin.path, "api/x.hurl");
    }

    #[test]
    fn a_workspace_pins_the_exact_commit_not_the_branch() {
        // Pinning to the branch would silently give a different tree on a
        // redownload, which is the whole reason the sha is recorded.
        let mut flow = RemoteFlow::new(RemoteKind::Workspace);
        flow.url = "https://git.test/o/r.git".into();
        flow.chosen_ref = Some(RefChoice {
            label: "main".into(),
            gitref: "refs/heads/main".into(),
        });
        flow.commit_sha = Some("deadbeef".into());
        flow.chosen_ws_filter = Some(WorkspaceGitFilter::HurlOnly);
        let origin = flow.workspace_origin().expect("origin");
        assert_eq!(origin.commit_sha, "deadbeef");
        assert_eq!(origin.ref_name, "main");
        assert_eq!(origin.filter, WorkspaceGitFilter::HurlOnly);
    }

    #[test]
    fn provenance_is_absent_rather_than_wrong_when_a_step_was_skipped() {
        let flow = RemoteFlow::new(RemoteKind::Collection);
        assert!(flow.git_origin().is_none());
        assert!(flow.workspace_origin().is_none());
    }
}
