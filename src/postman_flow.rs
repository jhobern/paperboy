//! The "import a Postman workspace" wizard, shared by both front-ends.
//!
//! [`crate::postman_api`] talks to Postman and [`crate::postman_import`] does
//! the downloading; this module holds the *flow* around them — which step comes
//! next, when a thread is spawned, what the user is asked before any of their
//! monthly API budget is spent — so the terminal UI and the GUI cannot disagree
//! about any of it. Each front-end supplies only presentation, exactly as with
//! [`crate::remote_flow`] and [`crate::save_flow`].
//!
//! The import runs as **one** worker thread across the whole wizard rather than
//! one per step. Postman's rate limits are learnt from the response headers of
//! the listing calls, and a fresh importer for the download phase would throw
//! that away and burst straight into a 429 — so the worker plans, parks on a
//! channel while the user reads the estimate, and downloads on the same
//! importer once they say go.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::i18n::Strings;
use crate::postman_api::{PostmanClient, WorkspaceKind, WorkspaceSummary};
use crate::postman_import::{
    ImportFormat, ImportMsg, ImportOptions, ImportPlan, ImportSummary, Importer, ItemKind,
    WaitReason, parse_workspace_ref,
};

/// Which step of the wizard the user is on.
///
/// [`Step::Failed`] carries its own message because every failure here is worth
/// reading — a rejected key, a workspace with nothing in it, a destination that
/// already exists — and none of them are recoverable by retrying blindly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// The API key, and the optional escape hatches: a workspace id to skip the
    /// listing entirely, and a base URL for tenants that aren't on
    /// `api.postman.com`.
    Connect,
    /// Choose from the workspaces the key can see.
    PickWorkspace,
    /// What to download, in what format, and where to put it.
    Options,
    /// The plan: how many items, how long it will take, and whether it would
    /// eat an uncomfortable share of the month's remaining API budget. Nothing
    /// bulk has been fetched yet at this point, so backing out here is free.
    Confirm,
    /// Downloading. [`PostmanFlow::progress`] drives the bar and the ETA.
    Downloading,
    Done,
    Failed(String),
}

/// What the flow is waiting on, so a busy screen can say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    ListingWorkspaces,
    Planning,
    Downloading,
}

impl Phase {
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            Phase::ListingWorkspaces => s.postman_busy_listing,
            Phase::Planning => s.postman_busy_planning,
            Phase::Downloading => s.postman_busy_downloading,
        }
    }
}

/// Live download progress, kept up to date from the importer's message channel.
#[derive(Debug, Clone, Default)]
pub(crate) struct Progress {
    pub(crate) done: usize,
    pub(crate) total: usize,
    /// The item currently being fetched, for a "3/60 · Billing API" line.
    pub(crate) current: String,
    /// What kind of thing `current` is, so the line can say "collection" or
    /// "environment" rather than leaving the user to guess from the name.
    pub(crate) current_kind: Option<ItemKind>,
    /// Set while the importer is deliberately idle, so a UI that looks stalled
    /// can explain itself instead of appearing hung.
    pub(crate) waiting: Option<(WaitReason, u64)>,
    started: Option<Instant>,
    /// How many of each kind the whole download will fetch, taken from the
    /// plan. Needed because the queue is not homogeneous: collections come
    /// first and cost more, so "seconds so far ÷ items so far" over-estimates
    /// for the whole of the collection run and then under-corrects.
    totals: [usize; 2],
    /// Time measured per kind, and how many samples it came from.
    spent: [Duration; 2],
    samples: [usize; 2],
    /// The item currently in flight: its kind, and when it started. Closed off
    /// into `spent`/`samples` when the next item begins.
    in_flight: Option<(ItemKind, Instant)>,
}

/// Index into the per-kind arrays on [`Progress`].
fn kind_slot(kind: ItemKind) -> usize {
    match kind {
        ItemKind::Collection => 0,
        ItemKind::Environment => 1,
    }
}

impl Progress {
    /// Note that `kind` has started, closing off the timing of whatever was in
    /// flight before it. Only finished items are measured; the one on screen
    /// is still running and would drag the average down.
    fn start_item(&mut self, kind: ItemKind) {
        let now = Instant::now();
        if let Some((prev, at)) = self.in_flight.take() {
            let slot = kind_slot(prev);
            self.spent[slot] += now.saturating_duration_since(at);
            self.samples[slot] += 1;
        }
        self.in_flight = Some((kind, now));
    }

    /// Average measured cost of one item of `kind`, falling back to the rate
    /// achieved over everything so far while that kind has no samples of its
    /// own (the environments have not started yet, and something is better
    /// than nothing).
    fn per_item(&self, kind: ItemKind, overall: Duration) -> Duration {
        let slot = kind_slot(kind);
        match self.samples[slot] {
            0 => overall,
            n => self.spent[slot] / n as u32,
        }
    }

    /// Time remaining based on the rate actually achieved so far, rather than
    /// the published one — a throttled account is slower than the estimate, and
    /// that is exactly when an ETA matters.
    ///
    /// Extrapolated per kind. A workspace of 23 collections and 500
    /// environments spends its first minute on the dear half of the queue, so
    /// a single blended average would quote a time for the cheap half that it
    /// measured on the dear one.
    pub(crate) fn eta(&self) -> Option<Duration> {
        let started = self.started?;
        if self.done == 0 || self.done >= self.total {
            return None;
        }
        let elapsed = Instant::now().saturating_duration_since(started);
        let overall = elapsed / self.done as u32;
        if self.totals.iter().sum::<usize>() != self.total {
            // No per-kind breakdown (an older plan, or a test that set only
            // the total): fall back to the blended rate.
            return Some(overall * (self.total - self.done) as u32);
        }
        let left = |kind: ItemKind| -> u32 {
            let slot = kind_slot(kind);
            self.totals[slot].saturating_sub(self.samples[slot]) as u32
        };
        Some(
            self.per_item(ItemKind::Collection, overall) * left(ItemKind::Collection)
                + self.per_item(ItemKind::Environment, overall) * left(ItemKind::Environment),
        )
    }

    pub(crate) fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.done as f32 / self.total as f32
    }
}

/// What the front-end must act on. Everything else the flow handles itself.
#[derive(Debug)]
pub(crate) enum PostmanEvent {
    /// The import finished. `dest` holds the folder to open as a workspace.
    Imported(Box<ImportSummary>),
}

/// A message from the worker thread. Progress arrives on its own channel; this
/// one carries only the outcomes the flow's step depends on.
enum Msg {
    Workspaces(Result<Vec<WorkspaceSummary>, String>),
    Planned(Box<ImportPlan>),
    Finished(Box<ImportSummary>),
    Failed(String),
}

pub(crate) struct PostmanFlow {
    // -- User input, written directly by the front-ends -------------------
    pub(crate) key: String,
    /// Optional. A workspace id or the address of the workspace in Postman;
    /// supplying one skips the listing step (and the API call it costs).
    pub(crate) workspace_ref: String,
    /// Optional. Blank means `api.postman.com`.
    pub(crate) base_url: String,
    /// Where the imported folder goes.
    pub(crate) dest: String,
    pub(crate) include_collections: bool,
    pub(crate) include_environments: bool,
    pub(crate) format: ImportFormat,
    pub(crate) overwrite: bool,
    /// Filter text for the workspace list.
    pub(crate) filter: String,
    /// Index into [`Self::visible_workspaces`].
    pub(crate) selected: usize,

    // -- Flow state -------------------------------------------------------
    step: Step,
    busy: Option<Phase>,
    /// When the current [`Self::busy`] phase started, so a long one can say how
    /// long it has been going. A listing that draws on Postman's strict rate
    /// limit can genuinely take minutes, and a spinner with no elapsed time (or
    /// no reason for the wait) is indistinguishable from a hang.
    busy_since: Option<Instant>,
    budget: Option<Budget>,
    workspaces: Vec<WorkspaceSummary>,
    chosen: Option<WorkspaceSummary>,
    plan: Option<ImportPlan>,
    progress: Progress,
    /// Items that could not be fetched, accumulated as they are reported so the
    /// running import can show them rather than only the final summary.
    failures: Vec<(String, String)>,

    // -- Worker -----------------------------------------------------------
    rx: Option<Receiver<Msg>>,
    progress_rx: Option<Receiver<ImportMsg>>,
    /// Released to let the parked worker start downloading. Dropping it instead
    /// is how a cancel at the confirmation step tells the worker to give up.
    go: Option<Sender<()>>,
    cancel: Arc<AtomicBool>,
    /// The last `{{ … }}` reference resolved, with what it resolved to.
    ///
    /// Resolving asks 1Password, which can put a fingerprint prompt on the
    /// screen; the listing, the plan and the download each build their own
    /// client, so without this one import would prompt three times. Keyed by
    /// the raw text so editing the field re-resolves rather than reusing the
    /// answer to a question that was since changed. Lives only as long as the
    /// wizard: the resolved secret is never persisted, exactly as an
    /// environment's resolved secret isn't.
    resolved_key: Arc<Mutex<Option<(String, String)>>>,
}

/// Where the Postman API key comes from.
///
/// The key field accepts a `{{ … }}` provider reference exactly as a `.vars`
/// file does, but knowing to type `{{ op://Vault/Item/credential }}` is a lot
/// to ask of someone whose job today is "import our Postman workspace". So the
/// front-ends ask *where the key lives* first, and then ask for the one piece
/// only the user knows — the item path, the parameter name, the variable name
/// — and assemble the reference themselves.
///
/// The wrapping is all this type does; resolving the finished reference is
/// still [`resolve_key`]'s job, so nothing downstream has to know the wizard
/// offered a choice at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum KeySource {
    /// The key itself, pasted in. Supported — it is what someone with the key
    /// on their clipboard expects, and what `$POSTMAN_API_KEY` seeds — but
    /// offered last and never the default: a pasted key is a live credential
    /// sitting in a text field, where a reference is only its address.
    Paste,
    /// The default. A 1Password item path is the safe answer, so it is the one
    /// the wizard proposes rather than one the user has to go looking for.
    #[default]
    OnePassword,
    Ssm,
    Env,
}

impl KeySource {
    /// Every source, in the order the front-ends offer them: the ones that
    /// keep the credential out of the app first, the pasted key last.
    pub(crate) const ALL: [KeySource; 4] = [
        KeySource::OnePassword,
        KeySource::Ssm,
        KeySource::Env,
        KeySource::Paste,
    ];

    /// The next/previous source, wrapping — for a left/right selector.
    pub(crate) fn cycled(self, forward: bool) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward { i + 1 } else { i + n - 1 } % n]
    }

    /// Whether what the user types is the credential itself (and so should be
    /// masked on screen). A reference is a *path* to a credential: masking it
    /// would only stop the user checking they typed it right.
    pub(crate) fn is_secret(self) -> bool {
        matches!(self, KeySource::Paste)
    }

    /// Wrap what the user typed into the reference the resolver understands.
    ///
    /// Tolerant of a user who already knows the syntax: an entry that is
    /// already a `{{ … }}` reference, or already carries its provider's
    /// prefix, is passed through rather than wrapped twice.
    pub(crate) fn reference(self, entry: &str) -> String {
        let entry = entry.trim();
        if entry.is_empty() || entry.contains("{{") {
            return entry.to_string();
        }
        match self {
            KeySource::Paste => entry.to_string(),
            KeySource::OnePassword => {
                let path = entry.strip_prefix("op://").unwrap_or(entry);
                format!("{{{{ op://{path} }}}}")
            }
            KeySource::Ssm => {
                let name = entry.strip_prefix("ssm:").unwrap_or(entry);
                format!("{{{{ ssm:{name} }}}}")
            }
            KeySource::Env => {
                let name = entry.strip_prefix("env:").unwrap_or(entry);
                format!("{{{{ env:{name} }}}}")
            }
        }
    }

    /// The inverse of [`Self::reference`]: which source a stored key came from,
    /// and what the user typed to make it. Lets a wizard reopened on an
    /// existing key show it the way it was entered rather than as raw syntax.
    pub(crate) fn detect(raw: &str) -> (Self, String) {
        let raw = raw.trim();
        // Nothing to go on yet: open on the source we would rather people used.
        if raw.is_empty() {
            return (KeySource::default(), String::new());
        }
        let Some(inner) = raw
            .strip_prefix("{{")
            .and_then(|r| r.strip_suffix("}}"))
            .map(str::trim)
        else {
            return (KeySource::Paste, raw.to_string());
        };
        if let Some(path) = inner.strip_prefix("op://") {
            (KeySource::OnePassword, path.to_string())
        } else if let Some(name) = inner.strip_prefix("ssm:") {
            (KeySource::Ssm, name.to_string())
        } else if let Some(name) = inner.strip_prefix("env:") {
            (KeySource::Env, name.to_string())
        } else {
            // Some other reference syntax: leave it exactly as typed rather
            // than mangling it into a source that doesn't fit.
            (KeySource::Paste, raw.to_string())
        }
    }
}

/// Turn whatever is in the key field into a key to send.
///
/// The field takes a `{{ … }}` provider reference — `{{ op://Private/Postman/
/// credential }}`, `{{ ssm:/path }}`, `{{ env:NAME }}` — exactly as a `.vars`
/// file does, so nobody has to fetch their key out of 1Password and paste a
/// live credential into a form to import a workspace. Anything else is already
/// a key and is used as typed.
///
/// Called on a worker thread, never on one that draws: resolving shells out to
/// `op`/`aws`, which can take seconds and put a fingerprint prompt on screen.
/// The answer is cached against the text it came from so an import asks once
/// rather than once per API phase.
fn resolve_key(raw: &str, cache: &Mutex<Option<(String, String)>>) -> Option<String> {
    let raw = raw.trim();
    // Nothing to ask anyone, and so nothing worth remembering.
    if !raw.contains("{{") {
        return (!raw.is_empty()).then(|| raw.to_string());
    }
    if let Ok(guard) = cache.lock()
        && let Some((cached_raw, value)) = guard.as_ref()
        && cached_raw == raw
    {
        return Some(value.clone());
    }
    let value = crate::environment::resolve_reference(raw)?;
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((raw.to_string(), value.clone()));
    }
    Some(value)
}

impl Default for PostmanFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl PostmanFlow {
    pub(crate) fn new() -> Self {
        Self {
            key: String::new(),
            workspace_ref: String::new(),
            base_url: String::new(),
            dest: String::new(),
            include_collections: true,
            include_environments: true,
            format: ImportFormat::default(),
            overwrite: false,
            filter: String::new(),
            selected: 0,
            step: Step::Connect,
            busy: None,
            busy_since: None,
            budget: None,
            workspaces: Vec::new(),
            chosen: None,
            plan: None,
            progress: Progress::default(),
            failures: Vec::new(),
            rx: None,
            progress_rx: None,
            go: None,
            cancel: Arc::new(AtomicBool::new(false)),
            resolved_key: Arc::new(Mutex::new(None)),
        }
    }

    /// Seed the key from `$POSTMAN_API_KEY`, as the headless import does, so
    /// someone who already has it exported doesn't retype it.
    pub(crate) fn with_env_key(mut self) -> Self {
        if let Ok(k) = std::env::var("POSTMAN_API_KEY")
            && !k.trim().is_empty()
        {
            self.key = k.trim().to_string();
        }
        self
    }

    // -- Inspection --------------------------------------------------------

    pub(crate) fn step(&self) -> &Step {
        &self.step
    }

    #[cfg(test)]
    pub(crate) fn busy(&self) -> Option<Phase> {
        self.busy
    }

    fn set_busy(&mut self, phase: Phase) {
        self.busy = Some(phase);
        self.busy_since = Some(Instant::now());
    }

    fn clear_busy(&mut self) {
        self.busy = None;
        self.busy_since = None;
        self.progress.waiting = None;
    }

    /// What to put beside the spinner: the phase, why it is waiting if it is,
    /// and how long it has been at it.
    ///
    /// The wait itself was already known — the pacer reports it — but only the
    /// download screen ever drew it, so a *listing* held back by Postman's rate
    /// limit sat on "Checking what that workspace holds…" for minutes looking
    /// like a hung app rather than a queue being observed.
    pub(crate) fn busy_line(&self, s: &Strings) -> Option<String> {
        let phase = self.busy?;
        let mut line = phase.label(s).to_string();
        if let Some((reason, secs)) = self.progress.waiting {
            let why = match reason {
                crate::postman_import::WaitReason::Pacing => s.postman_waiting_paced,
                crate::postman_import::WaitReason::RateLimited => s.postman_waiting_limited,
            };
            line.push_str(&format!(
                " \u{2014} {why} ({})",
                human_duration(Duration::from_secs(secs), s)
            ));
        }
        // Below a few seconds an elapsed counter is just noise; past that it is
        // the difference between "working" and "stuck".
        if let Some(started) = self.busy_since {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_secs(3) {
                line.push_str(&format!(" \u{b7} {}", human_duration(elapsed, s)));
            }
        }
        Some(line)
    }

    /// What Postman last said was left, ready to put under the progress bar.
    ///
    /// An import that is mostly waiting looks the same whether the account has
    /// two calls left this month or two hundred, and the estimate swings about
    /// as paced waits land inside it — so show the budget itself, which is a
    /// fact rather than an extrapolation.
    pub(crate) fn budget_line(&self, s: &Strings) -> Option<String> {
        let b = self.budget?;
        let mut parts: Vec<String> = Vec::new();
        if let Some(n) = b.remaining {
            let window = match b.reset_secs {
                Some(secs) if secs > 0 => format!(
                    "{} {} ({})",
                    n,
                    s.postman_budget_window,
                    human_duration(Duration::from_secs(secs), s)
                ),
                _ => format!("{n} {}", s.postman_budget_window),
            };
            parts.push(window);
        }
        if let Some(n) = b.remaining_month {
            parts.push(format!("{n} {}", s.postman_budget_month));
        }
        if b.interval_secs > 0 {
            parts.push(format!(
                "{} {}",
                s.postman_budget_pace,
                human_duration(Duration::from_secs(b.interval_secs), s)
            ));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "{}: {}",
            s.postman_budget_label,
            parts.join(" \u{b7} ")
        ))
    }

    /// The key reference worth remembering, or `None`.
    ///
    /// Only once the key has actually been *used* — Postman answered with a
    /// workspace list or a plan — so a half-typed item path is never offered
    /// back as a suggestion. A pasted key is never returned: it is the
    /// credential itself, and this ends up in `state.json`.
    pub(crate) fn key_to_remember(&self) -> Option<&str> {
        let proven = !self.workspaces.is_empty() || self.chosen.is_some() || self.plan.is_some();
        let key = self.key.trim();
        (proven && !key.is_empty() && !KeySource::detect(key).0.is_secret()).then_some(key)
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match &self.step {
            Step::Failed(e) => Some(e.as_str()),
            _ => None,
        }
    }

    pub(crate) fn plan(&self) -> Option<&ImportPlan> {
        self.plan.as_ref()
    }

    pub(crate) fn progress(&self) -> &Progress {
        &self.progress
    }

    pub(crate) fn failures(&self) -> &[(String, String)] {
        &self.failures
    }

    pub(crate) fn workspaces(&self) -> &[WorkspaceSummary] {
        &self.workspaces
    }

    /// The workspaces matching the filter, in display order. The filter is a
    /// case-insensitive substring so a long list can be narrowed by typing,
    /// matching how the git wizard filters files.
    pub(crate) fn visible_workspaces(&self) -> Vec<&WorkspaceSummary> {
        let needle = self.filter.trim().to_lowercase();
        self.workspaces
            .iter()
            .filter(|w| needle.is_empty() || w.name.to_lowercase().contains(&needle))
            .collect()
    }

    pub(crate) fn selected_workspace(&self) -> Option<&WorkspaceSummary> {
        self.visible_workspaces().get(self.selected).copied()
    }

    /// The name of the workspace being imported, once one is settled on.
    pub(crate) fn workspace_name(&self) -> &str {
        self.chosen.as_ref().map(|w| w.name.as_str()).unwrap_or("")
    }

    pub(crate) fn dest_path(&self) -> PathBuf {
        PathBuf::from(self.dest.trim())
    }

    fn options(&self) -> ImportOptions {
        ImportOptions {
            include_collections: self.include_collections,
            include_environments: self.include_environments,
            format: self.format,
            overwrite: self.overwrite,
        }
    }

    fn base_url_opt(&self) -> Option<String> {
        let t = self.base_url.trim();
        (!t.is_empty()).then(|| t.to_string())
    }

    // -- Advancing ---------------------------------------------------------

    /// Leave the first step. With a workspace id already in hand this skips
    /// straight to the options — the listing call costs a request on Postman's
    /// tightest rate-limit bucket, so it isn't made when it isn't needed.
    pub(crate) fn submit_connect(&mut self, s: &Strings) {
        if self.key.trim().is_empty() {
            self.step = Step::Failed(s.postman_err_key_required.to_string());
            return;
        }
        let typed = self.workspace_ref.trim().to_string();
        if !typed.is_empty() {
            let Some(id) = parse_workspace_ref(&typed) else {
                self.step = Step::Failed(s.postman_err_bad_workspace.to_string());
                return;
            };
            // Nothing has been listed, so the name isn't known — the id stands
            // in until the plan comes back with the real one.
            self.chosen = Some(WorkspaceSummary {
                id,
                name: typed,
                kind: WorkspaceKind::Other(String::new()),
            });
            self.step = Step::Options;
            return;
        }
        self.start_listing(s);
    }

    fn start_listing(&mut self, s: &Strings) {
        let (tx, rx) = mpsc::channel();
        let raw = self.key.trim().to_string();
        let base = self.base_url_opt();
        let cache = Arc::clone(&self.resolved_key);
        let bad_ref = s.postman_err_key_ref;
        thread::spawn(move || {
            let key = match resolve_key(&raw, &cache) {
                Some(k) => k,
                None => {
                    let _ = tx.send(Msg::Workspaces(Err(bad_ref.to_string())));
                    return;
                }
            };
            let client = PostmanClient::new(key, base);
            let kinds = WorkspaceKind::default_selection();
            let msg = match client.list_workspaces(&kinds) {
                Ok((mut ws, _rate)) => {
                    ws.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                    Msg::Workspaces(Ok(ws))
                }
                Err(e) => Msg::Workspaces(Err(e.to_string())),
            };
            let _ = tx.send(msg);
        });
        self.rx = Some(rx);
        self.set_busy(Phase::ListingWorkspaces);
        self.step = Step::PickWorkspace;
    }

    /// Take the highlighted workspace and move on to the options. Returns
    /// whether it advanced.
    pub(crate) fn submit_workspace(&mut self) -> bool {
        let Some(ws) = self.selected_workspace().cloned() else {
            return false;
        };
        self.chosen = Some(ws);
        self.step = Step::Options;
        true
    }

    /// Leave the options and start the worker, which plans and then parks
    /// waiting for [`Self::confirm`]. Returns whether it started.
    pub(crate) fn submit_options(&mut self, s: &Strings) -> bool {
        if self.dest.trim().is_empty() {
            self.step = Step::Failed(s.postman_err_dest_required.to_string());
            return false;
        }
        if !self.include_collections && !self.include_environments {
            self.step = Step::Failed(s.postman_err_nothing_selected.to_string());
            return false;
        }
        let Some(ws) = self.chosen.clone() else {
            self.step = Step::Failed(s.postman_err_no_workspace.to_string());
            return false;
        };

        let (tx, rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let raw = self.key.trim().to_string();
        let base = self.base_url_opt();
        let cache = Arc::clone(&self.resolved_key);
        let bad_ref = s.postman_err_key_ref;
        let options = self.options();
        let dest = self.dest_path();
        let cancel = Arc::clone(&self.cancel);
        cancel.store(false, Ordering::Relaxed);

        thread::spawn(move || {
            let key = match resolve_key(&raw, &cache) {
                Some(k) => k,
                None => {
                    let _ = tx.send(Msg::Failed(bad_ref.to_string()));
                    return;
                }
            };
            let client = PostmanClient::new(key, base);
            // One importer for the whole run: the pacer learns this account's
            // real budget from the listing calls, and starting over for the
            // download would throw that away and burst into a 429.
            let mut importer = Importer::new(&client)
                .with_progress(progress_tx)
                .with_cancel(cancel);

            let plan = match importer.plan(&ws.id, &ws.name, &options) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                    return;
                }
            };
            if tx.send(Msg::Planned(Box::new(plan.clone()))).is_err() {
                return; // the wizard was closed while planning
            }

            // Park until the user confirms. A dropped sender means they backed
            // out, which is a clean exit, not a failure.
            if go_rx.recv().is_err() {
                return;
            }

            match importer.download(&plan, &dest, &options) {
                Ok(summary) => {
                    let _ = tx.send(Msg::Finished(Box::new(summary)));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                }
            }
        });

        self.rx = Some(rx);
        self.progress_rx = Some(progress_rx);
        self.go = Some(go_tx);
        self.set_busy(Phase::Planning);
        self.step = Step::Confirm;
        true
    }

    /// Approve the plan and let the parked worker download. Returns whether it
    /// started.
    pub(crate) fn confirm(&mut self) -> bool {
        let Some(plan) = self.plan.as_ref() else {
            return false;
        };
        let total = plan.item_count();
        let Some(go) = self.go.take() else {
            return false;
        };
        if go.send(()).is_err() {
            return false; // the worker is already gone
        }
        self.progress = Progress {
            total,
            totals: [plan.collections.len(), plan.environments.len()],
            started: Some(Instant::now()),
            ..Progress::default()
        };
        self.set_busy(Phase::Downloading);
        self.step = Step::Downloading;
        true
    }

    /// Ask the worker to stop. Safe at any point: before the download it drops
    /// the go-ahead, during it sets the cancel flag the importer checks between
    /// calls (and which makes it clean up its staging folder).
    pub(crate) fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.go = None;
    }

    /// Go back to the workspace list, keeping it — the list belongs to the key
    /// that fetched it, and that hasn't changed.
    pub(crate) fn to_pick_workspace(&mut self) {
        self.chosen = None;
        self.plan = None;
        self.step = Step::PickWorkspace;
    }

    /// Return to the first step to fix the key or the URL, discarding the
    /// listing — a workspace list belongs to the key that fetched it.
    pub(crate) fn back_to_connect(&mut self) {
        self.cancel();
        self.rx = None;
        self.progress_rx = None;
        self.clear_busy();
        self.workspaces.clear();
        self.chosen = None;
        self.plan = None;
        self.selected = 0;
        self.filter.clear();
        self.step = Step::Connect;
    }

    /// Clear a failure without losing anything else, putting the user back on
    /// the step the failure interrupted — or, where that step has nothing to
    /// show, on the last one that has (see [`Self::recoverable`]).
    pub(crate) fn clear_error(&mut self, back_to: Step) {
        if matches!(self.step, Step::Failed(_)) {
            self.step = self.recoverable(back_to);
        }
    }

    /// The step to land on after dismissing an error.
    ///
    /// The step a failure interrupted is not always a step that can be drawn:
    /// a rejected API key fails *during* the workspace listing, so the
    /// interrupted step is "choose a workspace" — of a list that was never
    /// fetched. Dismissing the error dropped the user on an empty picker with
    /// no way forward, when the one thing they needed was the key prompt they
    /// had just typed into. Each step is therefore checked against what it
    /// needs to show, and falls back to the last step that has it.
    pub(crate) fn recoverable(&self, back_to: Step) -> Step {
        match back_to {
            Step::PickWorkspace if self.workspaces.is_empty() => Step::Connect,
            Step::Options | Step::Confirm | Step::Downloading if self.chosen.is_none() => {
                Step::Connect
            }
            // A download that failed cannot be resumed from its progress bar;
            // the options are where it is started from.
            Step::Confirm | Step::Downloading if self.plan.is_none() => Step::Options,
            Step::Downloading => Step::Options,
            other => other,
        }
    }

    pub(crate) fn fail(&mut self, message: String) {
        self.clear_busy();
        self.rx = None;
        self.progress_rx = None;
        self.go = None;
        self.step = Step::Failed(message);
    }

    // -- Polling -----------------------------------------------------------

    /// Collect whatever the worker has produced. Call each tick (the terminal
    /// UI) or each frame (the GUI).
    pub(crate) fn poll(&mut self, s: &Strings) -> Option<PostmanEvent> {
        self.drain_progress();

        let result = self.rx.as_ref().map(Receiver::try_recv)?;
        match result {
            Ok(msg) => self.apply(msg, s),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // The worker exits silently when the wizard cancels it, which
                // is not a failure; anything else really is one.
                self.rx = None;
                if self.busy.is_some() && !self.cancel.load(Ordering::Relaxed) {
                    self.fail(s.postman_err_worker_ended.to_string());
                }
                self.clear_busy();
                None
            }
        }
    }

    /// Fold every progress message that has arrived into [`Self::progress`].
    /// Drained separately from the outcome channel so a slow front-end can
    /// never fall behind the download.
    fn drain_progress(&mut self) {
        let Some(rx) = self.progress_rx.as_ref() else {
            return;
        };
        // Collected first so `self` isn't borrowed while it is mutated.
        let msgs: Vec<ImportMsg> = rx.try_iter().collect();
        for msg in msgs {
            match msg {
                ImportMsg::Item {
                    index,
                    total,
                    kind,
                    name,
                } => {
                    // `index` is 1-based and marks the *start* of an item, so
                    // the number finished is one fewer.
                    self.progress.done = index.saturating_sub(1);
                    self.progress.total = total;
                    self.progress.current = name;
                    self.progress.current_kind = Some(kind);
                    self.progress.waiting = None;
                    self.progress.start_item(kind);
                }
                ImportMsg::Waiting { reason, secs } => {
                    self.progress.waiting = Some((reason, secs));
                }
                ImportMsg::Budget {
                    remaining,
                    reset_secs,
                    remaining_month,
                    interval_secs,
                } => {
                    self.budget = Some(Budget {
                        remaining,
                        reset_secs,
                        remaining_month,
                        interval_secs,
                    });
                }
                ImportMsg::ItemFailed { name, error } => {
                    self.failures.push((name, error));
                }
                // The outcomes arrive on the other channel, where they can
                // change the step; here they would only race with it.
                ImportMsg::Listing
                | ImportMsg::Planned(_)
                | ImportMsg::Done(_)
                | ImportMsg::Failed(_) => {}
            }
        }
    }

    /// Every transition the worker can cause, split out from [`Self::poll`] so
    /// it can be tested without threads.
    fn apply(&mut self, msg: Msg, s: &Strings) -> Option<PostmanEvent> {
        match msg {
            Msg::Workspaces(Ok(ws)) => {
                self.clear_busy();
                self.rx = None;
                if ws.is_empty() {
                    // Not an error the user can fix by retrying: a Postman API
                    // key carries its owner's own access and cannot be scoped,
                    // so an empty list means an empty account.
                    self.fail(s.postman_err_no_workspaces.to_string());
                    return None;
                }
                self.workspaces = ws;
                self.selected = 0;
                None
            }
            Msg::Workspaces(Err(e)) => {
                self.fail(e);
                None
            }
            Msg::Planned(plan) => {
                self.clear_busy();
                // The plan carries the workspace's real name, which is all the
                // wizard had an id for when the user typed one in.
                if let Some(chosen) = self.chosen.as_mut()
                    && !plan.workspace_name.trim().is_empty()
                {
                    chosen.name = plan.workspace_name.clone();
                }
                self.plan = Some(*plan);
                None
            }
            Msg::Finished(summary) => {
                self.clear_busy();
                self.rx = None;
                self.progress_rx = None;
                self.progress.done = self.progress.total;
                self.progress.waiting = None;
                self.failures = summary.failures.clone();
                self.step = Step::Done;
                Some(PostmanEvent::Imported(summary))
            }
            Msg::Failed(e) => {
                self.fail(e);
                None
            }
        }
    }
}

/// Test-only seeding, so a front-end's tests can put a flow on the step they
/// care about without spawning threads or reaching a Postman API. Driving the
/// real transitions is [`PostmanFlow`]'s own job and is tested here.
#[cfg(test)]
impl PostmanFlow {
    pub(crate) fn seed_step(&mut self, step: Step) {
        self.step = step;
    }

    pub(crate) fn seed_workspaces(&mut self, workspaces: Vec<WorkspaceSummary>) {
        self.workspaces = workspaces;
    }

    pub(crate) fn seed_chosen(&mut self, workspace: WorkspaceSummary) {
        self.chosen = Some(workspace);
    }

    pub(crate) fn seed_plan(&mut self, plan: ImportPlan) {
        self.plan = Some(plan);
    }
}

/// A default folder name for the imported workspace, so the destination field
/// starts with something sensible rather than empty. Non-path characters are
/// replaced rather than dropped, so two differently-named workspaces cannot
/// collapse to the same folder.
pub(crate) fn default_dest_name(workspace: &str) -> String {
    let cleaned: String = workspace
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "Postman".to_string()
    } else {
        cleaned
    }
}

/// A short "3 collections, 2 environments" line for the confirmation step.
pub(crate) fn plan_summary(plan: &ImportPlan, s: &Strings) -> String {
    format!(
        "{} {} · {} {}",
        plan.collections.len(),
        s.postman_word_collections,
        plan.environments.len(),
        s.postman_word_environments
    )
}

/// Round a duration to something worth reading aloud. An import is paced in
/// whole seconds, so sub-second precision would be false precision.
/// What Postman's rate headers last reported, kept so the front-ends can show
/// the user how much of their allowance is left rather than only how long the
/// import thinks it has to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Budget {
    remaining: Option<u64>,
    reset_secs: Option<u64>,
    remaining_month: Option<u64>,
    interval_secs: u64,
}

pub(crate) fn human_duration(d: Duration, s: &Strings) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{} {}", secs.max(1), s.postman_unit_seconds)
    } else {
        let mins = secs.div_ceil(60);
        format!("{mins} {}", s.postman_unit_minutes)
    }
}

/// The label for one item kind, for the progress line.
pub(crate) fn item_kind_label(kind: ItemKind, s: &Strings) -> &'static str {
    match kind {
        ItemKind::Collection => s.postman_word_collection,
        ItemKind::Environment => s.postman_word_environment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::postman_api::ItemSummary;

    fn s() -> Strings {
        Strings::for_language(&Language::English)
    }

    #[test]
    fn a_key_source_wraps_what_the_user_typed_and_reads_it_back() {
        for src in KeySource::ALL {
            let (back, entry) = KeySource::detect(&src.reference("secret/thing"));
            assert_eq!(back, src, "{src:?} must survive the round trip");
            assert_eq!(entry, "secret/thing");
        }
        assert_eq!(
            KeySource::OnePassword.reference("Private/Postman/cred"),
            "{{ op://Private/Postman/cred }}"
        );
        assert_eq!(
            KeySource::Ssm.reference("/paperboy/postman"),
            "{{ ssm:/paperboy/postman }}"
        );
        assert_eq!(
            KeySource::Env.reference("POSTMAN_API_KEY"),
            "{{ env:POSTMAN_API_KEY }}"
        );
    }

    #[test]
    fn a_key_source_leaves_a_user_who_already_knows_the_syntax_alone() {
        // Already wrapped, or already prefixed: wrapping twice would break it.
        assert_eq!(
            KeySource::OnePassword.reference("{{ op://a/b/c }}"),
            "{{ op://a/b/c }}"
        );
        assert_eq!(
            KeySource::OnePassword.reference("op://a/b/c"),
            "{{ op://a/b/c }}"
        );
        assert_eq!(KeySource::Paste.reference(""), "");

        // A pasted key is not a reference, and an unfamiliar reference syntax
        // is kept verbatim rather than forced into a source that doesn't fit.
        assert_eq!(
            KeySource::detect("PMAK-abc"),
            (KeySource::Paste, "PMAK-abc".to_string())
        );
        assert_eq!(
            KeySource::detect("{{ vault:x }}"),
            (KeySource::Paste, "{{ vault:x }}".to_string())
        );
    }

    #[test]
    fn the_key_sources_cycle_both_ways_without_falling_off_the_end() {
        // 1Password leads: a reference is the encouraged answer, a pasted key
        // the supported-but-last one.
        assert_eq!(KeySource::default(), KeySource::OnePassword);
        assert_eq!(KeySource::OnePassword.cycled(false), KeySource::Paste);
        assert_eq!(KeySource::Paste.cycled(true), KeySource::OnePassword);
    }

    fn flow() -> PostmanFlow {
        let mut f = PostmanFlow::new();
        f.key = "PMAK-test".to_string();
        f.dest = "/tmp/pb-import-test".to_string();
        f
    }

    /// A key that is already a key is sent as typed — no provider, no prompt.
    #[test]
    fn a_typed_key_is_used_as_it_stands() {
        let cache = Mutex::new(None);
        assert_eq!(
            resolve_key("  PMAK-abcdef  ", &cache),
            Some("PMAK-abcdef".to_string())
        );
        assert!(
            cache.lock().unwrap().is_none(),
            "nothing to remember: no provider was asked"
        );
    }

    /// An import lists, plans and downloads, each building its own client. The
    /// provider must be asked once for all three: 1Password puts a fingerprint
    /// prompt on the screen, and three of them for one import is an interrogation.
    #[test]
    fn a_resolved_key_is_remembered_for_the_rest_of_the_import() {
        let raw = "{{ op://Private/Postman/credential }}";
        let cache = Mutex::new(Some((raw.to_string(), "PMAK-from-1password".to_string())));
        // Nothing here can reach `op`; an answer proves it came from the cache.
        assert_eq!(
            resolve_key(raw, &cache),
            Some("PMAK-from-1password".to_string())
        );
    }

    /// Edited text is a different question, so the remembered answer to the old
    /// one must not be handed back — that would import with a key the field no
    /// longer shows.
    #[test]
    fn editing_the_key_does_not_reuse_the_previous_answer() {
        let cache = Mutex::new(Some((
            "{{ op://Private/Postman/credential }}".to_string(),
            "PMAK-old".to_string(),
        )));
        assert_eq!(
            resolve_key("PMAK-typed-instead", &cache),
            Some("PMAK-typed-instead".to_string())
        );
    }

    fn ws(name: &str, id: &str) -> WorkspaceSummary {
        WorkspaceSummary {
            id: id.to_string(),
            name: name.to_string(),
            kind: WorkspaceKind::Team,
        }
    }

    fn plan_with(collections: usize, environments: usize) -> ImportPlan {
        ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "Billing".into(),
            collections: (0..collections)
                .map(|i| ItemSummary {
                    uid: format!("u{i}"),
                    id: format!("i{i}"),
                    name: format!("c{i}"),
                })
                .collect(),
            environments: (0..environments)
                .map(|i| ItemSummary {
                    uid: format!("e{i}"),
                    id: format!("e{i}"),
                    name: format!("e{i}"),
                })
                .collect(),
            remaining_month: None,
        }
    }

    /// The key is the one thing the import cannot proceed without, and saying
    /// so beats a rejected request several seconds later.
    #[test]
    fn connecting_without_a_key_is_refused_before_any_request() {
        let mut f = PostmanFlow::new();
        f.submit_connect(&s());
        assert_eq!(f.error(), Some(s().postman_err_key_required));
        assert!(f.rx.is_none(), "no worker was started");
    }

    /// Listing the workspaces costs a call on Postman's tightest rate-limit
    /// bucket, so a user who already knows the workspace skips it entirely.
    #[test]
    fn a_supplied_workspace_id_skips_the_listing_step() {
        let mut f = flow();
        f.workspace_ref =
            "https://go.postman.co/workspace/Team~11111111-2222-3333-4444-555555555555".to_string();
        f.submit_connect(&s());
        assert_eq!(*f.step(), Step::Options);
        assert!(!f.is_busy(), "nothing was fetched");
        assert_eq!(
            f.chosen.as_ref().unwrap().id,
            "11111111-2222-3333-4444-555555555555",
            "the id was taken out of the pasted address"
        );
    }

    /// Something that isn't an id or a Postman address is caught here rather
    /// than becoming a confusing 404 later.
    #[test]
    fn an_unrecognisable_workspace_reference_is_reported() {
        let mut f = flow();
        f.workspace_ref = "the billing one".to_string();
        f.submit_connect(&s());
        assert_eq!(f.error(), Some(s().postman_err_bad_workspace));
    }

    /// A key with no workspaces is a dead end, not an empty list to scroll: a
    /// Postman key carries its owner's own access and cannot be scoped.
    #[test]
    fn a_key_that_sees_no_workspaces_says_so() {
        let mut f = flow();
        f.busy = Some(Phase::ListingWorkspaces);
        f.apply(Msg::Workspaces(Ok(Vec::new())), &s());
        assert_eq!(f.error(), Some(s().postman_err_no_workspaces));
    }

    /// The filter narrows a long list, and the selection is read from the
    /// filtered view — picking the third visible row must not import the third
    /// row of the unfiltered one.
    #[test]
    fn the_selection_follows_the_filtered_list() {
        let mut f = flow();
        f.workspaces = vec![ws("Alpha", "a"), ws("Billing", "b"), ws("Beta", "c")];
        f.filter = "b".to_string();
        assert_eq!(f.visible_workspaces().len(), 2);
        f.selected = 1;
        assert_eq!(f.selected_workspace().unwrap().id, "c");
        assert!(f.submit_workspace());
        assert_eq!(*f.step(), Step::Options);
    }

    /// Downloading neither collections nor environments would produce an empty
    /// folder after spending API calls to find that out.
    #[test]
    fn importing_nothing_at_all_is_refused() {
        let mut f = flow();
        f.chosen = Some(ws("Billing", "b"));
        f.include_collections = false;
        f.include_environments = false;
        assert!(!f.submit_options(&s()));
        assert_eq!(f.error(), Some(s().postman_err_nothing_selected));
    }

    /// The download has to land somewhere, and the wizard is the place to say
    /// so — not the importer, several seconds and several calls later.
    #[test]
    fn a_missing_destination_is_refused() {
        let mut f = flow();
        f.chosen = Some(ws("Billing", "b"));
        f.dest = "   ".to_string();
        assert!(!f.submit_options(&s()));
        assert_eq!(f.error(), Some(s().postman_err_dest_required));
    }

    /// Nothing bulk is fetched until the user has seen the cost, so the plan
    /// arrives at its own step with the download still parked.
    #[test]
    fn the_plan_is_shown_before_anything_is_downloaded() {
        let mut f = flow();
        f.chosen = Some(ws("Billing", "b"));
        f.step = Step::Confirm;
        f.busy = Some(Phase::Planning);
        f.apply(Msg::Planned(Box::new(plan_with(3, 2))), &s());

        assert_eq!(*f.step(), Step::Confirm);
        assert!(!f.is_busy(), "the worker is parked, not working");
        assert_eq!(f.plan().unwrap().item_count(), 5);
        assert_eq!(
            plan_summary(f.plan().unwrap(), &s()),
            "3 collections · 2 environments"
        );
    }

    /// A typed-in id means the name isn't known until the plan comes back with
    /// it, so the wizard adopts the real one rather than showing a UUID.
    #[test]
    fn the_workspaces_real_name_replaces_a_typed_id() {
        let mut f = flow();
        f.chosen = Some(WorkspaceSummary {
            id: "11111111-2222-3333-4444-555555555555".into(),
            name: "11111111-2222-3333-4444-555555555555".into(),
            kind: WorkspaceKind::Other(String::new()),
        });
        f.apply(Msg::Planned(Box::new(plan_with(1, 0))), &s());
        assert_eq!(f.workspace_name(), "Billing");
    }

    /// Confirming with no plan — which a stray keypress could otherwise do —
    /// must not start anything.
    #[test]
    fn confirming_without_a_plan_does_nothing() {
        let mut f = flow();
        assert!(!f.confirm());
        assert_ne!(*f.step(), Step::Downloading);
    }

    /// Progress is folded from the importer's messages: `index` marks the item
    /// being *started*, so the count finished is one behind it.
    #[test]
    fn progress_counts_finished_items_not_started_ones() {
        let mut f = flow();
        let (tx, rx) = mpsc::channel();
        f.progress_rx = Some(rx);
        tx.send(ImportMsg::Item {
            index: 1,
            total: 4,
            kind: ItemKind::Collection,
            name: "A".into(),
        })
        .unwrap();
        f.drain_progress();
        assert_eq!(f.progress().done, 0, "the first item has only just begun");
        assert_eq!(f.progress().total, 4);
        assert_eq!(f.progress().current, "A");

        tx.send(ImportMsg::Item {
            index: 4,
            total: 4,
            kind: ItemKind::Environment,
            name: "D".into(),
        })
        .unwrap();
        f.drain_progress();
        assert_eq!(f.progress().done, 3);
    }

    /// A paced import spends most of its time deliberately idle. Saying so is
    /// the difference between "working" and "hung".
    #[test]
    fn a_deliberate_wait_is_reported_rather_than_looking_hung() {
        let mut f = flow();
        let (tx, rx) = mpsc::channel();
        f.progress_rx = Some(rx);
        tx.send(ImportMsg::Waiting {
            reason: WaitReason::RateLimited,
            secs: 12,
        })
        .unwrap();
        f.drain_progress();
        assert_eq!(f.progress().waiting, Some((WaitReason::RateLimited, 12)));

        // …and stops being reported the moment work resumes.
        tx.send(ImportMsg::Item {
            index: 2,
            total: 4,
            kind: ItemKind::Collection,
            name: "B".into(),
        })
        .unwrap();
        f.drain_progress();
        assert_eq!(f.progress().waiting, None);
    }

    /// A wait during *listing* used to be invisible: the pacer reported it, but
    /// only the download screen drew it, so a rate-limited listing sat on a
    /// bare phase label for minutes looking like a hung app.
    #[test]
    fn the_busy_line_says_why_it_is_waiting_not_just_what_it_is_doing() {
        let s = s();
        let mut f = flow();
        f.set_busy(Phase::Planning);
        assert_eq!(f.busy_line(&s).as_deref(), Some(Phase::Planning.label(&s)));

        let (tx, rx) = mpsc::channel();
        f.progress_rx = Some(rx);
        tx.send(ImportMsg::Waiting {
            reason: WaitReason::RateLimited,
            secs: 12,
        })
        .unwrap();
        f.drain_progress();
        let line = f.busy_line(&s).expect("still busy");
        assert!(
            line.contains(s.postman_waiting_limited),
            "the reason for the wait belongs on the line: {line}"
        );
    }

    /// "How much have we got left?" is the first question a slow import raises,
    /// and every response already answers it — the numbers were being folded
    /// into the pacer and then thrown away.
    #[test]
    fn the_allowance_postman_reports_is_shown_not_just_used_for_pacing() {
        let s = s();
        let mut f = flow();
        assert_eq!(f.budget_line(&s), None, "nothing to report before a call");

        let (tx, rx) = mpsc::channel();
        f.progress_rx = Some(rx);
        tx.send(ImportMsg::Budget {
            remaining: Some(8),
            reset_secs: Some(45),
            remaining_month: Some(812),
            interval_secs: 12,
        })
        .unwrap();
        f.drain_progress();
        let line = f.budget_line(&s).expect("a budget was reported");
        assert!(line.contains('8') && line.contains(s.postman_budget_window));
        assert!(line.contains("812") && line.contains(s.postman_budget_month));
        assert!(
            line.contains(s.postman_budget_pace),
            "the spacing explains the wait: {line}"
        );
    }

    /// One item failing must not read as the whole import failing: the folder
    /// is still produced, and the skipped items are listed.
    #[test]
    fn an_item_that_could_not_be_fetched_is_collected_not_fatal() {
        let mut f = flow();
        let (tx, rx) = mpsc::channel();
        f.progress_rx = Some(rx);
        tx.send(ImportMsg::ItemFailed {
            name: "Broken".into(),
            error: "404".into(),
        })
        .unwrap();
        f.drain_progress();
        assert_eq!(f.failures().len(), 1);
        assert_ne!(*f.step(), Step::Failed("404".into()));
    }

    /// Finishing hands the summary out exactly once, for the front-end to open
    /// the folder as a workspace.
    #[test]
    fn finishing_reports_the_summary_and_lands_on_done() {
        let mut f = flow();
        f.step = Step::Downloading;
        f.busy = Some(Phase::Downloading);
        let summary = ImportSummary {
            dest: PathBuf::from("/tmp/x"),
            workspace_name: "Billing".into(),
            collections: 3,
            environments: 2,
            failures: Vec::new(),
            converted_with_notes: false,
            elapsed: Duration::from_secs(4),
        };
        let event = f.apply(Msg::Finished(Box::new(summary)), &s());
        assert!(matches!(event, Some(PostmanEvent::Imported(_))));
        assert_eq!(*f.step(), Step::Done);
        assert!(!f.is_busy());
    }

    /// Going back to the key discards the workspaces fetched for the old one,
    /// so a workspace can never be chosen from a list belonging to another key.
    #[test]
    fn going_back_to_the_key_discards_the_listing() {
        let mut f = flow();
        f.workspaces = vec![ws("Alpha", "a")];
        f.chosen = Some(ws("Alpha", "a"));
        f.plan = Some(plan_with(1, 1));
        f.back_to_connect();
        assert_eq!(*f.step(), Step::Connect);
        assert!(f.workspaces().is_empty());
        assert!(f.plan().is_none());
    }

    /// The ETA is measured from the rate actually achieved, because a throttled
    /// account is slower than the published rate — which is exactly when
    /// someone wants to know how long is left.
    #[test]
    fn the_eta_extrapolates_from_the_measured_rate() {
        let p = Progress {
            done: 2,
            total: 10,
            started: Some(Instant::now() - Duration::from_secs(4)),
            ..Progress::default()
        };
        let eta = p.eta().expect("two of ten done is enough to extrapolate");
        // 2 items in 4s → 2s each → 8 left → ~16s.
        assert!(
            (14..=18).contains(&eta.as_secs()),
            "unexpected eta: {}s",
            eta.as_secs()
        );
        assert!((p.fraction() - 0.2).abs() < 0.001);
    }

    /// A rejected API key fails during the *listing*, so the step it interrupts
    /// is "choose a workspace" — of a list that was never fetched. Dismissing
    /// the error used to drop the user on an empty picker with nothing to pick
    /// and no way back; it returns them to the key they need to fix.
    #[test]
    fn dismissing_a_failed_listing_goes_back_to_the_key() {
        let mut f = PostmanFlow::new();
        f.key = "PMAK-wrong".to_string();
        f.step = Step::PickWorkspace;
        f.fail("401 Unauthorized".to_string());
        f.clear_error(Step::PickWorkspace);
        assert_eq!(*f.step(), Step::Connect);
        assert_eq!(f.key, "PMAK-wrong", "the key is kept, to be corrected");
    }

    /// A listing that did arrive is still worth going back to: only an empty
    /// picker is unusable.
    #[test]
    fn dismissing_an_error_keeps_a_workspace_list_that_was_fetched() {
        let mut f = PostmanFlow::new();
        f.workspaces = vec![ws("Alpha", "a")];
        f.step = Step::PickWorkspace;
        f.fail("something else".to_string());
        f.clear_error(Step::PickWorkspace);
        assert_eq!(*f.step(), Step::PickWorkspace);
    }

    /// A download cannot be resumed from its own progress bar, so a failure
    /// during it goes back to the options it was started from.
    #[test]
    fn a_failed_download_goes_back_to_the_options() {
        let mut f = PostmanFlow::new();
        f.chosen = Some(ws("Alpha", "a"));
        f.plan = Some(plan_with(1, 1));
        f.step = Step::Downloading;
        f.fail("connection reset".to_string());
        f.clear_error(Step::Downloading);
        assert_eq!(*f.step(), Step::Options);
    }

    /// A workspace of a few collections and hundreds of environments spends its
    /// first minute on the dear half of the queue. Extrapolating that rate over
    /// the cheap half is how "about two minutes" became a quarter of an hour,
    /// so each kind is extrapolated from its own measured rate.
    #[test]
    fn the_eta_extrapolates_each_kind_from_its_own_rate() {
        // 2 collections done, at 4s each; 200 environments left, measured at
        // 0.5s each from the one sample there is.
        let p = Progress {
            done: 3,
            total: 202,
            totals: [2, 200],
            spent: [Duration::from_secs(8), Duration::from_millis(500)],
            samples: [2, 1],
            started: Some(Instant::now() - Duration::from_secs(9)),
            ..Progress::default()
        };
        let eta = p.eta().expect("three done is enough to extrapolate");
        // 199 environments at 0.5s ≈ 100s. The blended rate (3s an item) would
        // have said ten minutes.
        assert!(
            (90..=110).contains(&eta.as_secs()),
            "unexpected eta: {}s",
            eta.as_secs()
        );
    }

    /// Nothing to extrapolate from yet, and nothing left to wait for, both mean
    /// "no ETA" rather than a misleading zero.
    #[test]
    fn there_is_no_eta_before_the_first_item_or_after_the_last() {
        let base = Progress {
            total: 10,
            started: Some(Instant::now()),
            ..Progress::default()
        };
        assert_eq!(
            Progress {
                done: 0,
                ..base.clone()
            }
            .eta(),
            None
        );
        assert_eq!(Progress { done: 10, ..base }.eta(), None);
    }

    /// A workspace name becomes a folder name, so anything a path can't hold is
    /// replaced rather than dropped — otherwise "A/B" and "AB" collide.
    #[test]
    fn the_default_folder_name_is_derived_from_the_workspace() {
        assert_eq!(default_dest_name("Billing API"), "Billing API");
        assert_eq!(default_dest_name("Team/Billing"), "Team-Billing");
        assert_eq!(default_dest_name("   "), "Postman");
    }

    #[test]
    fn durations_are_rounded_to_something_worth_reading() {
        let s = s();
        assert_eq!(human_duration(Duration::from_millis(200), &s), "1 seconds");
        assert_eq!(human_duration(Duration::from_secs(45), &s), "45 seconds");
        assert_eq!(human_duration(Duration::from_secs(61), &s), "2 minutes");
    }
}

/// An end-to-end run of the flow itself: real worker threads, real HTTP (to a
/// throwaway loopback server), real pacing. The step-level tests above feed
/// messages in by hand, which cannot catch the two things most likely to go
/// wrong here — the worker parking between planning and downloading, and the
/// progress channel being drained independently of the outcome one.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::i18n::Language;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A minimal stand-in for the Postman API: enough of `/workspaces`,
    /// `/collections` and `/environments` for a whole import to complete.
    fn stub_api() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                // Drain the headers so the client isn't left waiting on us.
                loop {
                    let mut h = String::new();
                    match reader.read_line(&mut h) {
                        Ok(0) => break,
                        Ok(_) if h.trim().is_empty() => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                let body = if path.starts_with("/workspaces/") {
                    r#"{"workspace":{"id":"ws-a","name":"Alpha","type":"team"}}"#.to_string()
                } else if path.starts_with("/workspaces") {
                    r#"{"workspaces":[{"id":"ws-a","name":"Alpha","type":"team"}]}"#.to_string()
                } else if path.starts_with("/collections/") {
                    r#"{"collection":{"info":{"name":"Billing","schema":"v2.1.0"},
                        "item":[{"name":"Get","request":{"method":"GET","url":{"raw":"https://x.test/a"}}}]}}"#
                        .to_string()
                } else if path.starts_with("/collections") {
                    r#"{"collections":[{"id":"c1","uid":"u-c1","name":"Billing"}],"meta":{"total":1}}"#
                        .to_string()
                } else if path.starts_with("/environments/") {
                    r#"{"environment":{"id":"e1","name":"Staging",
                        "values":[{"key":"HOST","value":"https://s.test","enabled":true}]}}"#
                        .to_string()
                } else if path.starts_with("/environments") {
                    r#"{"environments":[{"id":"e1","uid":"u-e1","name":"Staging"}]}"#.to_string()
                } else {
                    r#"{"error":{"message":"no"}}"#.to_string()
                };
                let mut sock = stream;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// Spin the flow like a front-end's event loop until `done` says so.
    fn pump(
        flow: &mut PostmanFlow,
        s: &Strings,
        done: impl Fn(&PostmanFlow) -> bool,
    ) -> Option<PostmanEvent> {
        for _ in 0..1200 {
            let event = flow.poll(s);
            if event.is_some() {
                return event;
            }
            if done(flow) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the flow never got there; step = {:?}", flow.step());
    }

    #[test]
    fn a_whole_workspace_imports_end_to_end_and_converts_to_hurl() {
        let s = Strings::for_language(&Language::English);
        let base = stub_api();
        let dest = std::env::temp_dir().join(format!("pb_flow_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let mut flow = PostmanFlow::new();
        flow.key = "PMAK-stub".to_string();
        flow.base_url = base;
        flow.dest = dest.to_string_lossy().into_owned();
        flow.format = ImportFormat::Hurl;

        // Step 1: no workspace id, so the listing really is fetched.
        flow.submit_connect(&s);
        pump(&mut flow, &s, |f| !f.is_busy());
        assert_eq!(flow.workspaces().len(), 1, "the listing arrived");

        // Step 2, then step 3: plan, but download nothing yet.
        assert!(flow.submit_workspace());
        assert!(flow.submit_options(&s));
        pump(&mut flow, &s, |f| f.plan().is_some());
        let plan = flow.plan().expect("a plan").clone();
        assert_eq!(plan.item_count(), 2, "one collection and one environment");
        assert_eq!(
            plan.workspace_name, "Alpha",
            "the plan carries the workspace's real name"
        );

        // Nothing has been written yet: the worker is parked, which is the
        // whole point of showing an estimate before spending the budget.
        assert!(!dest.exists(), "planning must not touch the disk");

        // Step 4: approve, and the *same* importer (with its learnt pacing)
        // does the download.
        assert!(flow.confirm());
        let event = pump(&mut flow, &s, |_| false).expect("the import finished");
        let PostmanEvent::Imported(summary) = event;

        assert_eq!(summary.collections, 1);
        assert_eq!(summary.environments, 1);
        assert!(summary.failures.is_empty(), "{:?}", summary.failures);
        assert_eq!(flow.step(), &Step::Done);
        assert!(
            dest.join("Collections/Billing.hurl").exists(),
            "the collection was converted to Hurl, not left as JSON"
        );
        assert!(dest.join("Environments/Staging.vars").exists());
        // Progress was reported, not just the final outcome.
        assert_eq!(flow.progress().total, 2);
        assert_eq!(flow.progress().done, 2);

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// Backing out at the confirmation step must spend nothing further and
    /// leave nothing on disk — the estimate exists so this is a real choice.
    #[test]
    fn cancelling_at_the_confirmation_downloads_nothing() {
        let s = Strings::for_language(&Language::English);
        let base = stub_api();
        let dest = std::env::temp_dir().join(format!("pb_flow_cancel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let mut flow = PostmanFlow::new();
        flow.key = "PMAK-stub".to_string();
        flow.base_url = base;
        flow.dest = dest.to_string_lossy().into_owned();
        // A supplied id skips the listing entirely — the call it would cost
        // sits on Postman's tightest bucket.
        flow.workspace_ref = "12345678-1234-1234-1234-123456789abc".to_string();
        flow.submit_connect(&s);
        assert_eq!(flow.step(), &Step::Options, "the listing was skipped");

        assert!(flow.submit_options(&s));
        pump(&mut flow, &s, |f| f.plan().is_some());
        flow.cancel();

        std::thread::sleep(Duration::from_millis(400));
        assert!(!dest.exists(), "cancelling must leave nothing behind");
    }
}
