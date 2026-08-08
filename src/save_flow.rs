//! The "save to a git remote" wizard, shared by both front-ends.
//!
//! The companion to [`crate::remote_flow`], and written the same way and for
//! the same reason: the terminal UI and the GUI each used to carry their own
//! copy of this flow, and the copies drifted — the GUI's could only push a
//! collection to a branch, while the terminal UI's had grown environments,
//! whole workspaces, reports and tag targets. Everything that decides *what
//! happens next* lives here; a front-end owns only how it looks.
//!
//! Never does a full checkout or clone. The background push reuses the same
//! blobless-fetch + `read-tree` plumbing the load flow uses (see
//! [`crate::git_remote::fetch_base`] / `commit_files` / `push_commit`),
//! touching only the file(s) actually being written.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::collection::Collection;
use crate::environment::Environment;
use crate::git_remote::{self, RefKind, RemoteRefs};
use crate::i18n::Strings;
use crate::remote_flow::WorkspaceGitFilter;

/// Whether the user is pushing to a branch or a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveTargetKind {
    Branch,
    Tag,
}

impl SaveTargetKind {
    /// The full ref (e.g. `refs/heads/main`) this target names.
    pub(crate) fn full_ref(self, name: &str) -> String {
        match self {
            SaveTargetKind::Branch => git_remote::branch_ref(name),
            SaveTargetKind::Tag => git_remote::tag_ref(name),
        }
    }

    /// Only a branch save repins the remembered origin: a tag is a snapshot,
    /// not somewhere later edits should follow.
    pub(crate) fn repins_origin(self) -> bool {
        self == SaveTargetKind::Branch
    }
}

/// Whether the name the user submitted matched an existing branch *at the time
/// they submitted it*.
///
/// This records intent only. The rule is re-checked freshly against the remote
/// immediately before pushing, so someone else creating a same-named ref in the
/// meantime can be reported as a race rather than silently committed onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetIntent {
    NewRef,
    ExistingBranch,
}

/// A push failure, typed so the two cases worth naming get a localized message
/// instead of a raw git error.
#[derive(Debug)]
pub(crate) enum SaveError {
    /// The tag already exists on the remote. Tags are never overwritten,
    /// regardless of when the app last looked.
    TagExists,
    /// The user meant to create a new branch, but by push time the name existed
    /// on the remote. Reported separately from an ordinary non-fast-forward so
    /// it isn't mistaken for one.
    RefExistsRace,
    /// Anything else (network, non-fast-forward, git missing, ...), already
    /// scrubbed of any access token.
    Other(String),
}

impl SaveError {
    pub(crate) fn message(self, s: &Strings) -> String {
        match self {
            SaveError::TagExists => s.git_tag_exists.to_string(),
            SaveError::RefExistsRace => s.git_ref_exists_race.to_string(),
            SaveError::Other(e) => e,
        }
    }
}

/// What is being pushed.
///
/// The index identifies which tab to update once the push lands; the content
/// itself is assembled by [`SaveFlow::payload`] from data the front-end passes
/// in, so this stays free of any front-end's storage layout.
#[derive(Debug, Clone)]
pub(crate) enum SaveSource {
    /// A collection tab, optionally accompanied by its environment as a second
    /// file in the same commit.
    Collection { ci: usize },
    /// A whole workspace folder previously downloaded from git: every file
    /// currently on disk under `root` is pushed as it sits there. There is no
    /// per-file path to choose, so [`Step::ChoosePaths`] is skipped. `filter`
    /// rides along so the repinned origin keeps the same download filter.
    Workspace {
        ci: usize,
        root: PathBuf,
        filter: WorkspaceGitFilter,
    },
    /// A PaperTrail `.trail` document. Like a collection it has one path to
    /// choose, but never an accompanying `.vars`.
    Report { report_idx: usize },
}

impl SaveSource {
    /// Whether this source picks its own file path(s). A workspace commits the
    /// tree as it stands, so it has nothing to ask.
    pub(crate) fn chooses_paths(&self) -> bool {
        !matches!(self, SaveSource::Workspace { .. })
    }

    /// Whether an environment can ride along in the same commit. Only a
    /// collection has one.
    pub(crate) fn can_include_env(&self) -> bool {
        matches!(self, SaveSource::Collection { .. })
    }
}

/// Which step of the wizard the user is on. Derived state — front-ends read it
/// rather than tracking their own copy, so the two can't disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Repo URL and an optional access token.
    Connect,
    /// The in-repo path(s) to write, and whether to include the environment.
    ChoosePaths,
    /// Branch-or-tag, plus the target name. The remote's existing refs arrive
    /// in the background and are offered as a picker once they do.
    ChooseTarget,
    /// Editing the commit message before pushing.
    CommitMessage,
    /// Pushed successfully.
    Done,
    /// A step failed. Shown until dismissed.
    Failed(String),
}

/// A background operation in flight, so a front-end can show what is happening
/// without inventing its own wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    FetchingRefs,
    Pushing,
}

impl Phase {
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            Phase::FetchingRefs => s.git_loading_refs,
            Phase::Pushing => s.git_save_pushing,
        }
    }
}

/// A completed background git operation.
enum SaveMsg {
    Refs(Result<RemoteRefs, String>),
    /// A finished push; `Ok` carries the new commit sha, so a workspace save can
    /// repin its origin to the exact commit rather than to "whatever the branch
    /// points at now".
    Pushed(Result<String, SaveError>),
}

/// Something the front-end must act on. Everything else — advancing a step,
/// recording an error, storing the fetched refs — the flow handles itself.
#[derive(Debug)]
pub(crate) enum SaveEvent {
    /// The push landed. The front-end should clear the tab's edit markers and,
    /// for a branch target, repin its remembered origin.
    Pushed { commit_sha: String },
}

/// The content of one file in the commit: its in-repo path and its text.
pub(crate) type SaveFile = (String, String);

/// The state machine behind the "save to git" wizard.
pub(crate) struct SaveFlow {
    pub(crate) source: SaveSource,
    /// The repo URL, prefilled from the remembered origin but editable.
    pub(crate) url: String,
    /// An optional access token. Never persisted anywhere.
    pub(crate) token: String,
    /// Where the collection/report file is written in the repo. Unused for a
    /// workspace, which commits its tree as-is.
    pub(crate) path: String,
    /// Where the accompanying `.vars` is written, when `include_env` is set.
    pub(crate) env_path: String,
    /// Whether the collection's environment rides along in the same commit.
    pub(crate) include_env: bool,
    pub(crate) target_kind: SaveTargetKind,
    pub(crate) target_name: String,
    pub(crate) message: String,
    /// The message to fall back to if the user clears the field entirely, so an
    /// empty commit message can never reach the remote.
    default_message: String,
    /// The full ref the item was originally loaded from — the base commit for a
    /// brand-new branch or tag. Always refetched fresh at push time; this is
    /// only the starting point, never a cached sha.
    origin_gitref: String,
    /// Recorded when [`Step::ChooseTarget`] is submitted, consumed by the push
    /// to tell a deliberate existing-branch commit from a new-ref race.
    intent: TargetIntent,
    /// The remote's refs, empty until the background fetch lands.
    refs: RemoteRefs,
    step: Step,
    busy: Option<Phase>,
    rx: Option<Receiver<SaveMsg>>,
}

impl SaveFlow {
    /// Build a flow for pushing collection `ci` back to where it came from.
    ///
    /// `env` is the collection's effective (linked, or active-global)
    /// environment, if it has one; when it does, the wizard offers to write it
    /// alongside. Requires `col.git_origin` — callers gate "Save to Git" on it.
    pub(crate) fn for_collection(ci: usize, col: &Collection, env: Option<&Environment>) -> Self {
        let origin = col.git_origin.clone();
        let env_path = env
            .and_then(|e| e.git_origin.as_ref())
            .map(|o| o.path.clone())
            .unwrap_or_else(|| {
                format!("{}.vars", env.map(|e| e.name.as_str()).unwrap_or(&col.name))
            });
        // A collection that was never loaded from git can still be pushed for
        // the first time; it just has nothing to prefill, so it gets a sensible
        // default path rather than a blank field.
        let default_path = default_repo_path(&col.name, col.path.as_deref(), "hurl");
        Self {
            include_env: env.is_some(),
            env_path,
            ..Self::base(
                SaveSource::Collection { ci },
                origin.as_ref(),
                default_path,
                &col.name,
            )
        }
    }

    /// Build a flow for pushing a whole git-loaded workspace tab back to its
    /// repo, seeded from its remembered origin.
    pub(crate) fn for_workspace(
        ci: usize,
        col: &Collection,
        origin: &crate::remote_flow::WorkspaceGitOrigin,
    ) -> Self {
        let root = col
            .workspace_root
            .clone()
            .expect("workspace git-save requires a workspace_root");
        let git_origin = crate::git_remote::GitOrigin {
            repo_url: origin.repo_url.clone(),
            path: String::new(),
            ref_kind: origin.ref_kind,
            ref_name: origin.ref_name.clone(),
        };
        Self::base(
            SaveSource::Workspace {
                ci,
                root,
                filter: origin.filter,
            },
            Some(&git_origin),
            String::new(),
            &col.name,
        )
    }

    /// Build a flow for pushing a PaperTrail report back to its repo.
    pub(crate) fn for_report(report_idx: usize, report: &crate::report::Report) -> Self {
        let origin = report.git_origin.clone();
        let default_path = default_repo_path(&report.name, report.path.as_deref(), "trail");
        Self::base(
            SaveSource::Report { report_idx },
            origin.as_ref(),
            default_path,
            &report.name,
        )
    }

    /// The seeding every source shares.
    ///
    /// The target name is prefilled with the branch it came from, defaulting the
    /// wizard to "add a commit to the branch we loaded from" — the common case —
    /// while staying editable. A tag origin starts blank instead, because
    /// reusing an existing tag name is exactly what is never allowed.
    fn base(
        source: SaveSource,
        origin: Option<&crate::git_remote::GitOrigin>,
        default_path: String,
        item_name: &str,
    ) -> Self {
        let default_message = format!("Update {item_name} via PaperBoy");
        let (target_kind, target_name, intent) = match origin.map(|o| o.ref_kind) {
            Some(RefKind::Tag) => (SaveTargetKind::Tag, String::new(), TargetIntent::NewRef),
            Some(RefKind::Branch) => (
                SaveTargetKind::Branch,
                origin.map(|o| o.ref_name.clone()).unwrap_or_default(),
                TargetIntent::ExistingBranch,
            ),
            // Never pushed anywhere before: default to the branch most repos
            // actually use, which the user can change before connecting.
            None => (
                SaveTargetKind::Branch,
                DEFAULT_BRANCH.to_string(),
                TargetIntent::ExistingBranch,
            ),
        };
        // Only used as the base for a *brand-new* ref; committing onto a branch
        // that already exists always refetches that branch's own tip instead.
        let origin_gitref = origin
            .map(|o| o.gitref())
            .unwrap_or_else(|| git_remote::branch_ref(&target_name));
        let path = origin
            .map(|o| o.path.clone())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(default_path);
        Self {
            source,
            url: origin.map(|o| o.repo_url.clone()).unwrap_or_default(),
            token: String::new(),
            path,
            env_path: String::new(),
            include_env: false,
            target_kind,
            target_name,
            message: default_message.clone(),
            default_message,
            origin_gitref,
            intent,
            refs: RemoteRefs::default(),
            step: Step::Connect,
            busy: None,
            rx: None,
        }
    }

    // -- Reading -----------------------------------------------------------

    pub(crate) fn step(&self) -> &Step {
        &self.step
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn busy(&self) -> Option<Phase> {
        self.busy
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// The remote's refs, or an empty listing while the fetch is still running.
    pub(crate) fn refs(&self) -> &RemoteRefs {
        &self.refs
    }

    /// The error to show, if the flow failed.
    pub(crate) fn error(&self) -> Option<&str> {
        match &self.step {
            Step::Failed(e) => Some(e.as_str()),
            _ => None,
        }
    }

    pub(crate) fn token_opt(&self) -> Option<String> {
        let t = self.token.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }

    /// The origin to remember after a successful branch push.
    pub(crate) fn pushed_origin(&self) -> crate::git_remote::GitOrigin {
        crate::git_remote::GitOrigin {
            repo_url: self.url.trim().to_string(),
            path: self.path.trim().to_string(),
            ref_kind: RefKind::Branch,
            ref_name: self.target_name.trim().to_string(),
        }
    }

    // -- Advancing ---------------------------------------------------------

    /// Leave the connect step. A workspace has no paths to choose, so it goes
    /// straight on to picking a ref (and starts fetching them).
    pub(crate) fn submit_connect(&mut self, s: &Strings) {
        if self.url.trim().is_empty() {
            self.step = Step::Failed(s.git_url_required.to_string());
            return;
        }
        if self.source.chooses_paths() {
            self.step = Step::ChoosePaths;
        } else {
            self.start_refs();
        }
    }

    /// Leave the path step, if the paths that are actually in use are usable.
    /// Returns whether it advanced.
    ///
    /// A blank path simply doesn't advance (the user is still typing), but a
    /// path that would escape the repository is called out, since silently
    /// doing nothing would look like a broken button.
    pub(crate) fn submit_paths(&mut self, s: &Strings) -> bool {
        if self.path.trim().is_empty() {
            return false;
        }
        if self.include_env && self.env_path.trim().is_empty() {
            return false;
        }
        let path = match clean_repo_path(&self.path, s) {
            Ok(p) => p,
            Err(e) => {
                self.step = Step::Failed(e);
                return false;
            }
        };
        let env_path = if self.include_env {
            match clean_repo_path(&self.env_path, s) {
                Ok(p) => p,
                Err(e) => {
                    self.step = Step::Failed(e);
                    return false;
                }
            }
        } else {
            self.env_path.clone()
        };
        self.path = path;
        self.env_path = env_path;
        self.start_refs();
        true
    }

    /// Record the chosen target and move on to the commit message. Returns
    /// whether it advanced.
    ///
    /// The intent is decided here, against the refs on screen, so that the
    /// fresh check at push time can tell a race from a deliberate choice.
    pub(crate) fn submit_target(&mut self) -> bool {
        let name = self.target_name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        self.intent = if self.target_kind == SaveTargetKind::Branch
            && self.refs.branches.iter().any(|b| b == &name)
        {
            TargetIntent::ExistingBranch
        } else {
            TargetIntent::NewRef
        };
        if self.message.trim().is_empty() {
            self.message = self.default_message.clone();
        }
        self.step = Step::CommitMessage;
        true
    }

    /// Assemble the commit and push it. `payload` is the content to write, built
    /// by [`Self::payload`] or its helpers. Returns whether the push started.
    pub(crate) fn submit_message(&mut self, payload: Result<Vec<SaveFile>, String>) -> bool {
        if self.message.trim().is_empty() {
            return false;
        }
        let files = match payload {
            Ok(files) => files,
            Err(e) => {
                self.step = Step::Failed(e);
                return false;
            }
        };
        self.busy = Some(Phase::Pushing);
        self.rx = Some(spawn_push(
            self.url.trim().to_string(),
            self.token_opt(),
            self.origin_gitref.clone(),
            self.target_kind,
            self.target_name.trim().to_string(),
            self.intent,
            files,
            self.message.clone(),
        ));
        true
    }

    /// Go back to the connect step, discarding any fetched refs (the URL or
    /// token may be about to change, which would invalidate them).
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn back_to_connect(&mut self) {
        self.refs = RemoteRefs::default();
        self.rx = None;
        self.busy = None;
        self.step = Step::Connect;
    }

    /// Clear a failure, returning to the step it happened on. Errors from the
    /// connect step return there, so the user can correct the URL.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub(crate) fn clear_error(&mut self) {
        if matches!(self.step, Step::Failed(_)) {
            self.step = Step::Connect;
        }
    }

    fn start_refs(&mut self) {
        self.busy = Some(Phase::FetchingRefs);
        self.rx = Some(spawn_refs(self.url.trim().to_string(), self.token_opt()));
        self.step = Step::ChooseTarget;
    }

    // -- Polling -----------------------------------------------------------

    /// Collect a finished background operation, if one has landed. Call each
    /// frame; returns `Some` only when the front-end has something to do.
    pub(crate) fn poll(&mut self, s: &Strings) -> Option<SaveEvent> {
        let msg = match self.rx.as_ref()?.try_recv() {
            Ok(msg) => msg,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                // The worker died without sending — don't leave the UI showing a
                // spinner that will never stop.
                self.rx = None;
                self.busy = None;
                return None;
            }
        };
        self.rx = None;
        self.apply(msg, s)
    }

    /// Every transition, split out from [`Self::poll`] so they can be tested
    /// without spawning a thread or reaching a network.
    fn apply(&mut self, msg: SaveMsg, s: &Strings) -> Option<SaveEvent> {
        self.busy = None;
        match msg {
            SaveMsg::Refs(Ok(refs)) => {
                self.refs = refs;
                None
            }
            SaveMsg::Refs(Err(e)) => {
                self.step = Step::Failed(e);
                None
            }
            SaveMsg::Pushed(Ok(commit_sha)) => {
                self.step = Step::Done;
                Some(SaveEvent::Pushed { commit_sha })
            }
            SaveMsg::Pushed(Err(err)) => {
                self.step = Step::Failed(err.message(s));
                None
            }
        }
    }

    // -- Payload -----------------------------------------------------------

    /// The files to commit for a collection save: the collection itself, plus
    /// its environment when the user asked for it.
    ///
    /// Refuses a collection with an empty file field, mirroring a local save: a
    /// blank path serializes to something PaperBoy can't read back, so pushing
    /// it would put an unopenable file in the repo.
    pub(crate) fn collection_payload(
        &self,
        col: &Collection,
        env: Option<&Environment>,
        s: &Strings,
    ) -> Result<Vec<SaveFile>, String> {
        if let Some((req, field)) = col.first_empty_file_field() {
            return Err(crate::i18n::Status::SaveUnreadableEmptyFile { req, field }.text(s));
        }
        let mut files = vec![(self.path.trim().to_string(), col.to_hurl())];
        if self.include_env
            && let Some(env) = env
        {
            files.push((self.env_path.trim().to_string(), env.to_vars_text()));
        }
        Ok(files)
    }

    /// The single file to commit for a report save: its source text, as-is.
    pub(crate) fn report_payload(&self, report: &crate::report::Report) -> Vec<SaveFile> {
        vec![(self.path.trim().to_string(), report.text.clone())]
    }

    /// Every file currently on disk under a workspace root.
    ///
    /// An empty result is an error rather than an empty commit: it almost
    /// certainly means the folder was moved or emptied behind the app's back,
    /// and pushing that would delete the collections in the repo.
    pub(crate) fn workspace_payload(root: &Path, s: &Strings) -> Result<Vec<SaveFile>, String> {
        match crate::workspace::collect_files_for_commit(root) {
            Ok(files) if files.is_empty() => Err(s.git_save_workspace_empty.to_string()),
            Ok(files) => Ok(files),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Test-only seeding, so a front-end's tests can put a flow on the step they
/// care about without spawning threads or reaching a network. Driving the real
/// transitions is [`SaveFlow`]'s own job and is tested here.
#[cfg(test)]
impl SaveFlow {
    pub(crate) fn seed_step(&mut self, step: Step) {
        self.step = step;
    }

    pub(crate) fn seed_refs_from(&mut self, refs: RemoteRefs) {
        self.refs = refs;
    }

    pub(crate) fn seed_intent(&mut self, intent: TargetIntent) {
        self.intent = intent;
    }

    pub(crate) fn intent(&self) -> TargetIntent {
        self.intent
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The branch to offer when nothing is remembered.
const DEFAULT_BRANCH: &str = "main";

/// Where to suggest writing an item that has never been pushed: the name of the
/// local file it came from, or a filename made from its display name.
pub(crate) fn default_repo_path(name: &str, local_path: Option<&Path>, ext: &str) -> String {
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

/// Normalize an in-repo path and refuse one that would escape the repository.
///
/// A commit is built by path, so a `..` or an absolute path would write outside
/// the tree the user thinks they are pushing to. Backslashes are folded to `/`
/// because a Windows-style path typed here is still a git path.
pub(crate) fn clean_repo_path(path: &str, s: &Strings) -> Result<String, String> {
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

// ---------------------------------------------------------------------------
// Background workers
// ---------------------------------------------------------------------------

fn spawn_refs(url: String, token: Option<String>) -> Receiver<SaveMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(SaveMsg::Refs(git_remote::refs_fresh(
            &url,
            token.as_deref(),
        )));
    });
    rx
}

/// Fetch the base commit fresh, write `files` on top of it (touching nothing
/// else), and push the result — never a full checkout, never `--force`.
///
/// `intent` plus a **fresh** ref listing re-validate the tag-is-never-
/// overwritten rule and detect a new-ref race right before anything is written,
/// so no decision here rests on a listing fetched minutes ago.
#[allow(clippy::too_many_arguments)]
fn spawn_push(
    url: String,
    token: Option<String>,
    origin_gitref: String,
    target_kind: SaveTargetKind,
    target_name: String,
    intent: TargetIntent,
    files: Vec<SaveFile>,
    message: String,
) -> Receiver<SaveMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| -> Result<String, SaveError> {
            let fresh = git_remote::refs_fresh(&url, token.as_deref()).map_err(SaveError::Other)?;
            if target_kind == SaveTargetKind::Tag && fresh.tags.iter().any(|t| t == &target_name) {
                return Err(SaveError::TagExists);
            }
            let is_existing_branch = target_kind == SaveTargetKind::Branch
                && fresh.branches.iter().any(|b| b == &target_name);
            if target_kind == SaveTargetKind::Branch
                && intent == TargetIntent::NewRef
                && is_existing_branch
            {
                return Err(SaveError::RefExistsRace);
            }
            // Commit onto the branch's own tip when appending to it, so the push
            // is a fast-forward; otherwise branch off where the item came from.
            let base_gitref = if is_existing_branch {
                git_remote::branch_ref(&target_name)
            } else {
                origin_gitref.clone()
            };
            let (repo, base_sha) = git_remote::fetch_base(&url, token.as_deref(), &base_gitref)
                .map_err(SaveError::Other)?;
            let (author_name, author_email) = git_remote::author_identity();
            let push_result = (|| -> Result<String, String> {
                let commit_sha = git_remote::commit_files(
                    &repo,
                    &base_sha,
                    &files,
                    &message,
                    &author_name,
                    &author_email,
                )?;
                git_remote::push_commit(
                    &url,
                    token.as_deref(),
                    &repo,
                    &commit_sha,
                    &target_kind.full_ref(&target_name),
                )?;
                Ok(commit_sha)
            })();
            git_remote::cleanup(&repo);
            push_result.map_err(SaveError::Other)
        })();
        let _ = tx.send(SaveMsg::Pushed(result));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    fn strings() -> Strings {
        Strings::for_language(&Language::English)
    }

    fn origin(ref_kind: RefKind, ref_name: &str, path: &str) -> crate::git_remote::GitOrigin {
        crate::git_remote::GitOrigin {
            repo_url: "https://example.test/repo.git".to_string(),
            path: path.to_string(),
            ref_kind,
            ref_name: ref_name.to_string(),
        }
    }

    fn collection(name: &str, ref_kind: RefKind, ref_name: &str) -> Collection {
        let mut col = Collection::new(name.to_string(), Vec::new());
        col.git_origin = Some(origin(ref_kind, ref_name, "api/orders.hurl"));
        col
    }

    fn flow() -> SaveFlow {
        SaveFlow::for_collection(0, &collection("Orders", RefKind::Branch, "main"), None)
    }

    /// The wizard opens pointed back at where the collection came from, so the
    /// common case — "commit this onto the branch I loaded it from" — needs no
    /// typing at all.
    #[test]
    fn a_collection_save_is_prefilled_from_where_it_came_from() {
        let f = flow();
        assert_eq!(f.url, "https://example.test/repo.git");
        assert_eq!(f.path, "api/orders.hurl");
        assert_eq!(f.target_kind, SaveTargetKind::Branch);
        assert_eq!(f.target_name, "main");
        assert_eq!(f.message, "Update Orders via PaperBoy");
        assert_eq!(*f.step(), Step::Connect);
    }

    /// A tag origin starts with a blank name on purpose: reusing an existing tag
    /// is the one thing this flow never allows, so offering it as the default
    /// would only lead the user into the error.
    #[test]
    fn a_tag_origin_does_not_prefill_the_tag_name() {
        let col = collection("Orders", RefKind::Tag, "v1.0");
        let f = SaveFlow::for_collection(0, &col, None);
        assert_eq!(f.target_kind, SaveTargetKind::Tag);
        assert_eq!(f.target_name, "");
        assert_eq!(f.intent, TargetIntent::NewRef);
    }

    #[test]
    fn connecting_without_a_url_is_refused() {
        let s = strings();
        let mut f = flow();
        f.url = "   ".to_string();
        f.submit_connect(&s);
        assert_eq!(f.error(), Some(s.git_url_required));
    }

    /// A workspace commits the tree exactly as it sits on disk, so there is no
    /// per-file path to ask about — it goes straight to choosing a ref.
    #[test]
    fn a_workspace_save_skips_the_path_step() {
        let s = strings();
        let mut col = Collection::new("WS".to_string(), Vec::new());
        col.workspace_root = Some(std::env::temp_dir().join("pb-save-flow-test"));
        let ws_origin = crate::remote_flow::WorkspaceGitOrigin {
            repo_url: "https://example.test/repo.git".to_string(),
            commit_sha: "abc123".to_string(),
            ref_kind: RefKind::Branch,
            ref_name: "main".to_string(),
            filter: WorkspaceGitFilter::HurlAndJson,
        };
        let mut f = SaveFlow::for_workspace(3, &col, &ws_origin);
        assert!(!f.source.chooses_paths());
        assert!(!f.source.can_include_env());
        f.submit_connect(&s);
        assert_eq!(*f.step(), Step::ChooseTarget);
        assert_eq!(f.busy(), Some(Phase::FetchingRefs));
    }

    #[test]
    fn a_collection_save_asks_for_paths_first() {
        let s = strings();
        let mut f = flow();
        f.submit_connect(&s);
        assert_eq!(*f.step(), Step::ChoosePaths);
    }

    /// A blank path would write to the repo root under an empty name, so the
    /// step simply refuses to advance rather than pushing something broken.
    #[test]
    fn a_blank_path_does_not_advance() {
        let mut f = flow();
        f.path = "  ".to_string();
        assert!(!f.submit_paths(&strings()));
        assert_eq!(*f.step(), Step::Connect);
    }

    /// The environment path only matters when the environment is actually being
    /// included, so a blank one mustn't block a collection-only save.
    #[test]
    fn a_blank_env_path_only_blocks_when_the_env_is_included() {
        let mut f = flow();
        f.env_path = String::new();
        f.include_env = true;
        assert!(!f.submit_paths(&strings()));
        f.include_env = false;
        assert!(f.submit_paths(&strings()));
    }

    /// Whether the user meant "add to this branch" or "make a new one" is
    /// decided from what was on screen when they chose, so the fresh check at
    /// push time can tell a race apart from a deliberate choice.
    /// A path with a `..` in it would write outside the repository the user
    /// believes they are pushing to, so it is refused by name rather than
    /// silently doing nothing.
    #[test]
    fn a_path_that_escapes_the_repository_is_refused() {
        let s = strings();
        for bad in ["../secrets.hurl", "/etc/passwd", "a/../../b.hurl"] {
            let mut f = flow();
            f.path = bad.to_string();
            assert!(!f.submit_paths(&s), "{bad} should not be accepted");
            assert_eq!(f.error(), Some(s.gui_git_err_path_relative), "{bad}");
        }
    }

    /// The same protection applies to the environment riding along in the
    /// commit, which is a separate path the user can type.
    #[test]
    fn an_environment_path_that_escapes_the_repository_is_refused() {
        let s = strings();
        let mut f = flow();
        f.include_env = true;
        f.env_path = "../../dev.vars".to_string();
        assert!(!f.submit_paths(&s));
        assert_eq!(f.error(), Some(s.gui_git_err_path_relative));
    }

    /// Paths are normalized on the way through, so a leading `./` or a
    /// Windows-style separator still lands where the user meant.
    #[test]
    fn a_path_is_tidied_up_before_it_is_used() {
        let s = strings();
        let mut f = flow();
        f.path = "./api\\orders.hurl".to_string();
        assert!(f.submit_paths(&s));
        assert_eq!(f.path, "api/orders.hurl");
    }

    /// An item that has never been pushed anywhere still gets a usable
    /// starting point rather than a blank form.
    #[test]
    fn a_collection_never_pushed_before_still_gets_a_sensible_default() {
        let col = Collection::new("My API".to_string(), Vec::new());
        let f = SaveFlow::for_collection(0, &col, None);
        assert_eq!(f.url, "");
        assert_eq!(f.path, "My API.hurl");
        assert_eq!(f.target_name, "main");
    }

    #[test]
    fn a_default_path_prefers_the_local_file_it_came_from() {
        assert_eq!(
            default_repo_path(
                "Ignored",
                Some(Path::new("/home/u/apis/orders.hurl")),
                "hurl"
            ),
            "orders.hurl"
        );
        assert_eq!(
            default_repo_path("a/b", None, "hurl"),
            "a_b.hurl",
            "a name with separators cannot become a folder by accident"
        );
    }

    #[test]
    fn choosing_an_existing_branch_records_that_intent() {
        let mut f = flow();
        f.refs = RemoteRefs {
            branches: vec!["main".to_string(), "dev".to_string()],
            tags: vec![],
        };
        f.target_name = "main".to_string();
        assert!(f.submit_target());
        assert_eq!(f.intent, TargetIntent::ExistingBranch);
        assert_eq!(*f.step(), Step::CommitMessage);
    }

    #[test]
    fn choosing_an_unknown_name_is_recorded_as_a_new_ref() {
        let mut f = flow();
        f.refs = RemoteRefs {
            branches: vec!["main".to_string()],
            tags: vec![],
        };
        f.target_name = "feature/new".to_string();
        assert!(f.submit_target());
        assert_eq!(f.intent, TargetIntent::NewRef);
    }

    /// A tag never counts as "existing" for intent purposes — a tag that already
    /// exists is an outright error at push time, not a branch-style append.
    #[test]
    fn a_tag_target_is_always_a_new_ref() {
        let mut f = flow();
        f.target_kind = SaveTargetKind::Tag;
        f.refs = RemoteRefs {
            branches: vec![],
            tags: vec!["v1.0".to_string()],
        };
        f.target_name = "v1.0".to_string();
        assert!(f.submit_target());
        assert_eq!(f.intent, TargetIntent::NewRef);
    }

    /// Clearing the commit message entirely must not produce a blank commit —
    /// the default comes back rather than the field staying empty.
    #[test]
    fn a_cleared_commit_message_falls_back_to_the_default() {
        let mut f = flow();
        f.message = "   ".to_string();
        f.target_name = "main".to_string();
        assert!(f.submit_target());
        assert_eq!(f.message, "Update Orders via PaperBoy");
    }

    #[test]
    fn an_empty_target_name_does_not_advance() {
        let mut f = flow();
        f.target_name = "  ".to_string();
        assert!(!f.submit_target());
    }

    /// Going back to edit the URL throws away the refs fetched for the old one,
    /// so a target can never be chosen from a listing belonging to a different
    /// repository.
    #[test]
    fn going_back_to_connect_discards_the_fetched_refs() {
        let mut f = flow();
        f.refs = RemoteRefs {
            branches: vec!["main".to_string()],
            tags: vec![],
        };
        assert!(!f.refs.branches.is_empty());
        f.back_to_connect();
        assert!(f.refs.branches.is_empty() && f.refs.tags.is_empty());
        assert_eq!(*f.step(), Step::Connect);
    }

    #[test]
    fn a_successful_push_reports_the_new_commit() {
        let s = strings();
        let mut f = flow();
        let event = f.apply(SaveMsg::Pushed(Ok("deadbeef".to_string())), &s);
        assert!(matches!(
            event,
            Some(SaveEvent::Pushed { commit_sha }) if commit_sha == "deadbeef"
        ));
        assert_eq!(*f.step(), Step::Done);
        assert!(!f.is_busy());
    }

    /// The two failures worth naming get their own wording; anything else is
    /// shown as git reported it.
    #[test]
    fn the_named_push_failures_get_their_own_message() {
        let s = strings();
        for (err, want) in [
            (SaveError::TagExists, s.git_tag_exists),
            (SaveError::RefExistsRace, s.git_ref_exists_race),
        ] {
            let mut f = flow();
            assert!(f.apply(SaveMsg::Pushed(Err(err)), &s).is_none());
            assert_eq!(f.error(), Some(want));
        }
        let mut f = flow();
        let raw = SaveError::Other("could not resolve host".to_string());
        assert!(f.apply(SaveMsg::Pushed(Err(raw)), &s).is_none());
        assert_eq!(f.error(), Some("could not resolve host"));
    }

    #[test]
    fn a_failed_ref_listing_is_shown_rather_than_silently_ignored() {
        let s = strings();
        let mut f = flow();
        assert!(
            f.apply(SaveMsg::Refs(Err("repository not found".into())), &s)
                .is_none()
        );
        assert_eq!(f.error(), Some("repository not found"));
        f.clear_error();
        assert_eq!(f.error(), None);
    }

    /// Only a branch save repins where the item lives. A tag is a snapshot, so
    /// later edits must keep following the branch, not the tag.
    #[test]
    fn only_a_branch_target_repins_the_remembered_origin() {
        assert!(SaveTargetKind::Branch.repins_origin());
        assert!(!SaveTargetKind::Tag.repins_origin());
    }

    #[test]
    fn the_remembered_origin_uses_the_target_that_was_pushed_to() {
        let mut f = flow();
        f.url = "  https://example.test/repo.git  ".to_string();
        f.path = " api/orders.hurl ".to_string();
        f.target_name = " release ".to_string();
        let o = f.pushed_origin();
        assert_eq!(o.repo_url, "https://example.test/repo.git");
        assert_eq!(o.path, "api/orders.hurl");
        assert_eq!(o.ref_name, "release");
        assert_eq!(o.ref_kind, RefKind::Branch);
    }

    #[test]
    fn a_collection_payload_carries_the_environment_only_when_asked() {
        let s = strings();
        let col = collection("Orders", RefKind::Branch, "main");
        let env = Environment {
            id: 1,
            name: "Dev".to_string(),
            vars: Vec::new(),
            path: None,
            git_origin: None,
        };
        let mut f = SaveFlow::for_collection(0, &col, Some(&env));

        assert!(
            f.include_env,
            "an attached environment is offered by default"
        );
        let files = f.collection_payload(&col, Some(&env), &s).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "api/orders.hurl");
        assert_eq!(files[1].0, "Dev.vars");

        f.include_env = false;
        let files = f.collection_payload(&col, Some(&env), &s).unwrap();
        assert_eq!(files.len(), 1);
    }

    /// Pushing a collection with an empty file field would put a file in the
    /// repo that PaperBoy itself cannot reopen, so it is refused exactly as a
    /// local save is.
    #[test]
    fn a_collection_that_cannot_be_read_back_is_not_pushed() {
        let s = strings();
        let mut col = collection("Orders", RefKind::Branch, "main");
        let mut entry = crate::hurl::HurlEntry::default();
        entry.title = "Upload".to_string();
        entry.form_fields.push(crate::hurl::FormField {
            key: "avatar".to_string(),
            value: String::new(),
            kind: crate::hurl::FormFieldKind::File,
            content_type: None,
            base64_prefix: None,
            enabled: true,
            desc: String::new(),
        });
        col.entries.push(entry);
        assert!(
            col.first_empty_file_field().is_some(),
            "the collection really does have an unfilled file field"
        );

        let f = SaveFlow::for_collection(0, &col, None);
        assert!(
            f.collection_payload(&col, None, &s).is_err(),
            "a collection PaperBoy could not reopen is refused"
        );
    }

    #[test]
    fn a_workspace_with_nothing_in_it_is_refused_rather_than_emptying_the_repo() {
        let s = strings();
        let empty = std::env::temp_dir().join(format!(
            "paperboy-save-flow-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&empty).unwrap();
        let err = SaveFlow::workspace_payload(&empty, &s).unwrap_err();
        assert_eq!(err, s.git_save_workspace_empty);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn a_report_payload_is_its_source_text_at_the_chosen_path() {
        let mut report = crate::report::Report::from_text("Nightly", "# a trail\n");
        report.git_origin = Some(origin(RefKind::Branch, "main", "trails/nightly.trail"));
        let f = SaveFlow::for_report(2, &report);
        assert_eq!(f.message, "Update Nightly via PaperBoy");
        let files = f.report_payload(&report);
        assert_eq!(
            files,
            vec![(
                "trails/nightly.trail".to_string(),
                "# a trail\n".to_string()
            )]
        );
    }

    #[test]
    fn a_blank_token_is_treated_as_no_token() {
        let mut f = flow();
        f.token = "   ".to_string();
        assert_eq!(f.token_opt(), None);
        f.token = " ghp_secret ".to_string();
        assert_eq!(f.token_opt().as_deref(), Some("ghp_secret"));
    }
}
