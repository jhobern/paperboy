//! Bulk import of a whole Postman workspace into a folder PaperBoy can open
//! as a workspace.
//!
//! This is the engine only: it knows nothing about the TUI, the GUI or the
//! CLI. It takes a plan, talks to [`crate::postman_api`], reports progress
//! down an [`mpsc`] channel and leaves a finished folder on disk. All three
//! front-ends drive the same code, so they cannot drift apart.
//!
//! Three things make this more than a `for` loop over the API:
//!
//! * **Pacing.** Postman rate-limits two groups of endpoints at very
//!   different rates (see [`RateBucket`]). Going too fast earns a 429 and a
//!   forced wait that is slower than pacing correctly in the first place;
//!   going uniformly slow (as the shell-script approach does) makes a large
//!   workspace take four times longer than it needs to.
//! * **Partial results.** A 60-collection import that dies on collection 59
//!   because one collection was deleted mid-run is worse than useless. Only
//!   errors that doom the whole run — a bad key, the monthly cap, the user
//!   cancelling — stop it; anything else is recorded and reported at the end.
//! * **Atomicity.** The destination folder appears complete or not at all, so
//!   an interrupted import never leaves something that looks like a workspace
//!   but is missing half its collections.

// Nothing drives this engine yet — the CLI and the two wizards come next.
// Remove this once a front-end calls `Importer`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::postman::ConversionNote;
use crate::postman_api::{
    ApiError, COLLECTION_FETCH_COST, ENVIRONMENT_FETCH_COST, GENERAL_MIN_INTERVAL, ItemSummary,
    PostmanClient, RateInfo, STRICT_MIN_INTERVAL, sanitize_file_name, unique_file_name,
};

/// Subfolder for collections, matching the layout the original backup script
/// produced so an existing backup folder and a PaperBoy import look the same.
pub const COLLECTIONS_DIR: &str = "Collections";
/// Subfolder for environments.
pub const ENVIRONMENTS_DIR: &str = "Environments";

/// Where an [`ImportFormat::Hurl`] import writes what it could not convert.
pub const NOTES_FILE: &str = "CONVERSION-NOTES.md";

/// How many times a single item is retried before it is recorded as failed.
/// Kept low deliberately: with dozens of items, a long retry chain on each is
/// how an import turns into an apparent hang.
const MAX_RETRIES: u32 = 3;

/// How many item fetches are allowed to be in flight at once.
///
/// Chosen against the general budget rather than the link: at one call every
/// 200ms, six in flight covers about a second of round-trip latency before the
/// pacer becomes the limit again, which is the point at which more workers buy
/// nothing. A burst is never larger than the rate limiter already allows, since
/// every worker still takes its slot from the pacer.
const FETCH_CONCURRENCY: usize = 6;

/// Ceiling on a single rate-limit wait. Postman occasionally reports a reset
/// far in the future; blocking on it silently for minutes looks like a freeze,
/// so the wait is capped and simply retried.
const MAX_WAIT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Clock seam
// ---------------------------------------------------------------------------

/// Time and sleeping, injected so the pacer can be tested without actually
/// waiting. Production uses [`RealClock`]; tests use a virtual clock that
/// records what *would* have been slept.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

// ---------------------------------------------------------------------------
// Pacer
// ---------------------------------------------------------------------------

/// Which of Postman's two rate-limit budgets a call draws on.
///
/// Re-exported from the API module so callers of the engine don't need both
/// imports.
pub use crate::postman_api::RateBucket;

/// Keeps requests inside Postman's published rates, and adapts when the
/// server's own headers say the budget is tighter than expected.
///
/// The two buckets are tracked separately because they are separate budgets:
/// spending the strict one on listing collections does not slow down the
/// general one used to fetch them.
pub struct Pacer {
    strict_next: Option<Instant>,
    general_next: Option<Instant>,
    strict_interval: Duration,
    general_interval: Duration,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    pub fn new() -> Self {
        Pacer {
            strict_next: None,
            general_next: None,
            strict_interval: STRICT_MIN_INTERVAL,
            general_interval: GENERAL_MIN_INTERVAL,
        }
    }

    fn slot(&mut self, bucket: RateBucket) -> (&mut Option<Instant>, Duration) {
        match bucket {
            RateBucket::Strict => (&mut self.strict_next, self.strict_interval),
            RateBucket::General => (&mut self.general_next, self.general_interval),
        }
    }

    fn set_next(&mut self, bucket: RateBucket, at: Instant) {
        match bucket {
            RateBucket::Strict => self.strict_next = Some(at),
            RateBucket::General => self.general_next = Some(at),
        }
    }

    /// How long a call on `bucket` must wait before it may be sent.
    pub fn delay_before(&mut self, bucket: RateBucket, now: Instant) -> Duration {
        let (next, _) = self.slot(bucket);
        match *next {
            Some(t) if t > now => t.saturating_duration_since(now),
            _ => Duration::ZERO,
        }
    }

    /// Block until a call on `bucket` may be sent, then reserve the following
    /// slot. Returns how long it waited, so the caller can tell the user why
    /// nothing appeared to happen.
    #[cfg(test)]
    pub fn wait(&mut self, bucket: RateBucket, clock: &dyn Clock) -> Duration {
        let delay = self.reserve(bucket, clock.now());
        if !delay.is_zero() {
            clock.sleep(delay);
        }
        delay
    }

    /// Claim the next slot on `bucket` and say how long the caller must wait
    /// before using it — *without* sleeping.
    ///
    /// Split out from [`Pacer::wait`] because several fetches now run at once:
    /// they take their slots one at a time under a lock and then wait for them
    /// in parallel. Sleeping inside the lock would serialise them again, which
    /// is the whole thing being fixed.
    pub fn reserve(&mut self, bucket: RateBucket, now: Instant) -> Duration {
        let delay = self.delay_before(bucket, now);
        let (next, interval) = self.slot(bucket);
        // Reserve from the moment the call actually goes out, not from the
        // moment it was requested, or a slow response would let the next call
        // fire immediately and burst.
        *next = Some(now + delay + interval);
        delay
    }

    /// Fold the server's own accounting back into the schedule.
    ///
    /// The published rates are a floor, not a promise: a shared team account
    /// can be closer to its limit than a fresh one. When the headers say `n`
    /// calls remain before a reset in `t` seconds, spreading them over `t`
    /// keeps the import moving instead of sprinting into a 429 and stalling.
    pub fn observe(&mut self, bucket: RateBucket, info: &RateInfo, now: Instant) {
        let base = bucket.min_interval();
        let (remaining, reset) = (info.remaining, info.reset_secs);
        let (Some(remaining), Some(reset)) = (remaining, reset) else {
            return;
        };
        let reset = Duration::from_secs(reset.min(MAX_WAIT.as_secs()));

        if remaining == 0 {
            // Budget spent: nothing may go out until the window turns over.
            self.set_next(bucket, now + reset);
            return;
        }

        let spread = reset / (remaining as u32);
        let interval = spread.max(base);
        match bucket {
            RateBucket::Strict => self.strict_interval = interval,
            RateBucket::General => self.general_interval = interval,
        }
    }

    /// The interval currently being kept on `bucket` — how far apart calls are
    /// being spaced after whatever the server's headers last said.
    pub fn interval(&self, bucket: RateBucket) -> Duration {
        match bucket {
            RateBucket::Strict => self.strict_interval,
            RateBucket::General => self.general_interval,
        }
    }

    /// Honour an explicit 429: nothing on this bucket may go out for `secs`.
    pub fn back_off(&mut self, bucket: RateBucket, secs: Option<u64>, now: Instant) -> Duration {
        let wait = secs
            .map(|s| Duration::from_secs(s).min(MAX_WAIT))
            .unwrap_or(Duration::from_secs(5));
        self.set_next(bucket, now + wait);
        wait
    }
}

// ---------------------------------------------------------------------------
// Plan and estimation
// ---------------------------------------------------------------------------

/// One workspace's share of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePlan {
    pub workspace_id: String,
    pub workspace_name: String,
    pub collections: Vec<ItemSummary>,
    pub environments: Vec<ItemSummary>,
}

impl WorkspacePlan {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty() && self.environments.is_empty()
    }
}

/// What an import will fetch, worked out before any bulk downloading starts so
/// the user can be shown a cost and an ETA and given the chance to back out.
///
/// An import covers one workspace or many (see [`Importer::plan_all`]). The
/// flat `collections`/`environments` lists are every workspace's items
/// concatenated, workspace by workspace, so everything that only wants totals,
/// an ETA or a preview list can ignore the grouping entirely; only the writer
/// cares which workspace an item came from, because that decides its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    /// The workspaces this import covers, in the order they are downloaded.
    pub workspaces: Vec<WorkspacePlan>,
    pub collections: Vec<ItemSummary>,
    pub environments: Vec<ItemSummary>,
    /// Workspaces that were asked for but could not be listed, as
    /// `(name, reason)`. Only an import of many workspaces can have these: one
    /// workspace the key has lost access to must not cost the other forty.
    pub skipped: Vec<(String, String)>,
    /// `RateLimit-Remaining-Month` as of the listing calls, when Postman sent
    /// it. Lets the confirmation step warn that an import would eat most of a
    /// user's monthly budget before it spends any of it.
    pub remaining_month: Option<u64>,
}

impl ImportPlan {
    /// Build a plan from the per-workspace listings, flattening them once so
    /// the two views can never drift apart.
    pub fn new(
        workspaces: Vec<WorkspacePlan>,
        skipped: Vec<(String, String)>,
        remaining_month: Option<u64>,
    ) -> Self {
        let collections = workspaces
            .iter()
            .flat_map(|w| w.collections.iter().cloned())
            .collect();
        let environments = workspaces
            .iter()
            .flat_map(|w| w.environments.iter().cloned())
            .collect();
        ImportPlan {
            workspaces,
            collections,
            environments,
            skipped,
            remaining_month,
        }
    }

    /// Whether this import spans more than one workspace, which is what
    /// decides the on-disk layout.
    pub fn is_multi(&self) -> bool {
        self.workspaces.len() > 1
    }

    /// The workspace name, when there is exactly one. Empty for an import of
    /// several, which belongs to no single workspace.
    pub fn workspace_name(&self) -> &str {
        match self.workspaces.as_slice() {
            [only] => &only.workspace_name,
            _ => "",
        }
    }
    /// Total number of items to download.
    pub fn item_count(&self) -> usize {
        self.collections.len() + self.environments.len()
    }

    /// API calls the download phase will make. The listing calls are already
    /// spent by the time a plan exists, so they are not counted.
    pub fn api_calls(&self) -> usize {
        self.item_count()
    }

    /// Roughly how long the download will take.
    ///
    /// Counted per kind, because the two do not cost the same: a collection is
    /// a whole document and a environment is a short list of variables, and the
    /// round trip — not the pacing interval — is what most of the time is spent
    /// on. Pacing alone would put a 500-environment import at under two
    /// minutes; the same import measured itself at over ten.
    ///
    /// Still an estimate, not a promise: a throttled account is slower again,
    /// which is why the running import reports a measured ETA too (see
    /// `Progress::eta`), and why the wizard says "about".
    pub fn estimated_duration(&self) -> Duration {
        COLLECTION_FETCH_COST * self.collections.len() as u32
            + ENVIRONMENT_FETCH_COST * self.environments.len() as u32
    }

    /// Whether this import would consume an uncomfortable share of the
    /// month's remaining API budget, so the UI can say so before starting.
    pub fn strains_monthly_budget(&self) -> bool {
        match self.remaining_month {
            Some(rem) => self.api_calls() as u64 * 4 > rem,
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Options, messages, results
// ---------------------------------------------------------------------------

/// The on-disk form imported items take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportFormat {
    /// Postman's own JSON, byte for byte. PaperBoy opens `.json` collections
    /// and Postman environment exports directly, so this needs no conversion
    /// and cannot lose anything.
    #[default]
    Raw,
    /// Converted to Hurl: collections become `.hurl`, environments and
    /// collection-level variables become `.vars`. This is the form to pick when
    /// the point of the import is to stop depending on Postman — but Hurl
    /// doesn't cover everything Postman does, so anything dropped is listed in
    /// `CONVERSION-NOTES.md` at the root of the imported folder rather than
    /// disappearing quietly.
    Hurl,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub include_collections: bool,
    pub include_environments: bool,
    pub format: ImportFormat,
    /// Replace `dest` if it already exists. Off by default so a mistyped
    /// destination cannot destroy an unrelated folder.
    pub overwrite: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            include_collections: true,
            include_environments: true,
            format: ImportFormat::Raw,
            overwrite: false,
        }
    }
}

/// Which kind of item a progress message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Collection,
    Environment,
}

/// Why the import is currently not making requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// Staying inside the published rate — expected, and not a problem.
    Pacing,
    /// The server returned 429; this wait was imposed, not chosen.
    RateLimited,
}

/// Progress reported to whichever front-end is driving the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportMsg {
    /// Listing the workspace's contents, before the plan is known.
    Listing,
    /// The plan is known; the front-end can now show a count and an estimate.
    Planned(Box<ImportPlan>),
    /// Starting to fetch one item. `index` is 1-based for display.
    Item {
        index: usize,
        total: usize,
        kind: ItemKind,
        name: String,
    },
    /// Deliberately idle. Reported so a stalled-looking UI can explain itself
    /// rather than appear hung.
    Waiting {
        reason: WaitReason,
        secs: u64,
    },
    /// What the server's own rate headers said after the last call, plus the
    /// spacing they bought. Reported because "how much have we got left?" is
    /// the first question a slow import raises, and the answer is already in
    /// every response — it was simply being folded into the pacer and thrown
    /// away.
    Budget {
        /// Which of Postman's two allowances these numbers describe. Sent
        /// because they are separate accounts — a listing call and a fetch
        /// call answer with unrelated figures — and a reader shown them
        /// interleaved would watch the same "calls left" jump between 9 and
        /// 283 with nothing to say why.
        bucket: RateBucket,
        /// Calls left in the current window, and how long until it resets.
        remaining: Option<u64>,
        reset_secs: Option<u64>,
        /// Calls left in the account's monthly allowance.
        remaining_month: Option<u64>,
        /// Seconds currently being left between calls on this bucket.
        interval_secs: u64,
    },
    /// One item could not be fetched; the import continued without it.
    ItemFailed {
        name: String,
        error: String,
    },
    Done(Box<ImportSummary>),
    Failed(String),
}

/// What an import actually produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub dest: PathBuf,
    pub workspace_name: String,
    pub collections: usize,
    pub environments: usize,
    /// Items that could not be fetched, with the reason. Empty on a clean run.
    pub failures: Vec<(String, String)>,
    /// Whether a `CONVERSION-NOTES.md` was written — i.e. the conversion to
    /// Hurl left something behind that is worth reading about. Always false for
    /// [`ImportFormat::Raw`], which converts nothing.
    pub converted_with_notes: bool,
    pub elapsed: Duration,
}

impl ImportSummary {
    /// Test-only in this crate: the front-ends read `failures` directly, since
    /// they need the count and the reasons rather than just a yes or no.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// A failure that ends the whole import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    Api(ApiError),
    /// The workspace listed no collections and no environments. Postman
    /// answers an unknown workspace id with an empty list rather than a 404,
    /// so this doubles as "no such workspace".
    Empty,
    DestNotEmpty(PathBuf),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Api(e) => write!(f, "{e}"),
            ImportError::Empty => write!(
                f,
                "that workspace has no collections or environments, or the id is wrong"
            ),
            ImportError::DestNotEmpty(p) => {
                write!(f, "{} already exists and is not empty", p.display())
            }
            ImportError::Io(m) => write!(f, "could not write the import: {m}"),
            ImportError::Cancelled => write!(f, "import cancelled"),
        }
    }
}

impl From<ApiError> for ImportError {
    fn from(e: ApiError) -> Self {
        ImportError::Api(e)
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Runs one import. Created per import, not reused.
pub struct Importer<'a> {
    client: &'a PostmanClient,
    /// Behind a lock because the download runs several fetches at once and they
    /// all draw on the same rate-limit budget: the pacer is the one place that
    /// decides when the next call may go out, whoever is asking.
    pacer: Mutex<Pacer>,
    clock: &'a dyn Clock,
    cancel: Arc<AtomicBool>,
    /// How many item fetches may be in flight at once. Settable so a test can
    /// pin it to one and get a deterministic call order.
    concurrency: usize,
    /// A `Sender` is `Send` but not `Sync`, and the fetch workers share one
    /// `&Importer`; the lock is what makes that legal. It is held only for the
    /// length of a `send`.
    progress: Mutex<Option<Sender<ImportMsg>>>,
}

impl<'a> Importer<'a> {
    pub fn new(client: &'a PostmanClient) -> Self {
        Importer {
            client,
            pacer: Mutex::new(Pacer::new()),
            clock: &RealClock,
            cancel: Arc::new(AtomicBool::new(false)),
            concurrency: FETCH_CONCURRENCY,
            progress: Mutex::new(None),
        }
    }

    /// Swap in a fake clock so the pacing waits can be asserted without a test
    /// actually sleeping through them.
    ///
    /// Also pins the fetches to one at a time: the fake transports answer in
    /// *call* order, so overlapping fetches would hand a test's second scripted
    /// response to whichever item won the race. Tests about concurrency itself
    /// ask for it back with [`Importer::with_concurrency`].
    #[cfg(test)]
    pub fn with_clock(mut self, clock: &'a dyn Clock) -> Self {
        self.clock = clock;
        self.concurrency = 1;
        self
    }

    /// Pin how many fetches overlap.
    #[cfg(test)]
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_progress(mut self, tx: Sender<ImportMsg>) -> Self {
        self.progress = Mutex::new(Some(tx));
        self
    }

    fn send(&self, msg: ImportMsg) {
        let guard = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = guard.as_ref() {
            // A closed channel means the front-end has gone away; the
            // cancellation flag is the way that is reported, so a send error
            // is not itself a failure.
            let _ = tx.send(msg);
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn check_cancel(&self) -> Result<(), ImportError> {
        if self.cancelled() {
            Err(ImportError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Pace a call, telling the front-end if the wait is long enough to be
    /// visible. Sub-second pacing is not worth a message.
    fn pace(&self, bucket: RateBucket) {
        let waited = self.locked_pacer().reserve(bucket, self.clock.now());
        if !waited.is_zero() {
            self.clock.sleep(waited);
        }
        if waited >= Duration::from_secs(1) {
            self.send(ImportMsg::Waiting {
                reason: WaitReason::Pacing,
                secs: waited.as_secs(),
            });
        }
    }

    /// Fold the server's accounting into the pacer *and* pass it on, so the
    /// user can see the same numbers the pacer is reacting to.
    fn observe(&self, bucket: RateBucket, rate: &RateInfo) {
        let interval = {
            let mut pacer = self.locked_pacer();
            pacer.observe(bucket, rate, self.clock.now());
            pacer.interval(bucket)
        };
        self.send(ImportMsg::Budget {
            bucket,
            remaining: rate.remaining,
            reset_secs: rate.reset_secs,
            remaining_month: rate.remaining_month,
            interval_secs: interval.as_secs(),
        });
    }

    /// The pacer, recovering from a panicked holder rather than propagating it:
    /// a poisoned schedule is still a usable schedule, and the alternative is
    /// failing an import over a lock.
    fn locked_pacer(&self) -> std::sync::MutexGuard<'_, Pacer> {
        self.pacer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Work out what an import of `workspace_id` would fetch.
    ///
    /// This is the expensive-per-call half: listing collections draws on the
    /// strict bucket. It is separated from [`Importer::download`] so a UI can
    /// show the plan and let the user confirm before the bulk work starts.
    pub fn plan(
        &mut self,
        workspace_id: &str,
        workspace_name: &str,
        options: &ImportOptions,
    ) -> Result<ImportPlan, ImportError> {
        self.send(ImportMsg::Listing);
        self.check_cancel()?;

        let mut remaining_month = None;
        let one =
            self.list_workspace(workspace_id, workspace_name, options, &mut remaining_month)?;
        if one.is_empty() {
            return Err(ImportError::Empty);
        }

        let plan = ImportPlan::new(vec![one], Vec::new(), remaining_month);
        self.send(ImportMsg::Planned(Box::new(plan.clone())));
        Ok(plan)
    }

    /// Work out what an import of *several* workspaces would fetch.
    ///
    /// Listing is per workspace, so this costs two calls per workspace before
    /// anything is downloaded — which is exactly why the result still goes to
    /// a confirmation step rather than straight to the download.
    ///
    /// A workspace that lists nothing is dropped rather than reported: an
    /// account that has an empty scratch workspace should still be able to
    /// import the rest of itself in one go. A workspace the key has lost
    /// access to is recorded in [`ImportPlan::skipped`] for the same reason.
    /// Anything else — a bad key, an exhausted monthly budget — would fail for
    /// every remaining workspace too, so it stops the whole plan.
    pub fn plan_all(
        &mut self,
        workspaces: &[(String, String)],
        options: &ImportOptions,
    ) -> Result<ImportPlan, ImportError> {
        self.send(ImportMsg::Listing);
        self.check_cancel()?;

        let mut remaining_month = None;
        let mut planned: Vec<WorkspacePlan> = Vec::new();
        let mut skipped: Vec<(String, String)> = Vec::new();

        for (id, name) in workspaces {
            self.check_cancel()?;
            match self.list_workspace(id, name, options, &mut remaining_month) {
                Ok(w) if w.is_empty() => {}
                Ok(w) => planned.push(w),
                Err(ImportError::Api(e)) if workspace_failure_is_survivable(&e) => {
                    skipped.push((display_workspace(id, name), e.to_string()));
                }
                Err(e) => return Err(e),
            }
        }

        if planned.is_empty() {
            return Err(ImportError::Empty);
        }

        let plan = ImportPlan::new(planned, skipped, remaining_month);
        self.send(ImportMsg::Planned(Box::new(plan.clone())));
        Ok(plan)
    }

    /// List one workspace's collections and environments. Returns an empty
    /// plan rather than an error when the workspace holds nothing, because
    /// what that means depends on whether it was the only one asked for.
    fn list_workspace(
        &mut self,
        workspace_id: &str,
        workspace_name: &str,
        options: &ImportOptions,
        remaining_month: &mut Option<u64>,
    ) -> Result<WorkspacePlan, ImportError> {
        let collections = if options.include_collections {
            self.pace(RateBucket::Strict);
            let (items, rate) =
                self.retrying(RateBucket::Strict, |c| c.list_collections(workspace_id))?;
            *remaining_month = rate.remaining_month.or(*remaining_month);
            self.observe(RateBucket::Strict, &rate);
            items
        } else {
            Vec::new()
        };

        self.check_cancel()?;

        let environments = if options.include_environments {
            self.pace(RateBucket::General);
            let (items, rate) =
                self.retrying(RateBucket::General, |c| c.list_environments(workspace_id))?;
            *remaining_month = rate.remaining_month.or(*remaining_month);
            self.observe(RateBucket::General, &rate);
            items
        } else {
            Vec::new()
        };

        Ok(WorkspacePlan {
            workspace_id: workspace_id.to_string(),
            workspace_name: workspace_name.to_string(),
            collections,
            environments,
        })
    }

    /// Fetch everything in `plan` and leave it at `dest`.
    ///
    /// Everything is written to a staging folder beside `dest` and moved into
    /// place in one step at the end, so `dest` never exists in a half-imported
    /// state — including if the process is killed mid-run.
    pub fn download(
        &mut self,
        plan: &ImportPlan,
        dest: &Path,
        options: &ImportOptions,
    ) -> Result<ImportSummary, ImportError> {
        // Every exit from this call reports its outcome exactly once. An early
        // `?` that skipped the `Failed` message would leave a consumer waiting
        // for a message that never arrives — which is a hang, not an error.
        let result = self.download_inner(plan, dest, options);
        match &result {
            Ok(summary) => self.send(ImportMsg::Done(Box::new(summary.clone()))),
            Err(e) => self.send(ImportMsg::Failed(e.to_string())),
        }
        result
    }

    fn download_inner(
        &mut self,
        plan: &ImportPlan,
        dest: &Path,
        options: &ImportOptions,
    ) -> Result<ImportSummary, ImportError> {
        let started = self.clock.now();
        ensure_destination(dest, options.overwrite)?;
        let staging = staging_path(dest);
        // A staging folder left by a previous crashed run is stale by
        // definition; the plan it was built from is gone.
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(io_err)?;

        let result = self.download_into(plan, &staging, options, started);

        match result {
            Ok(mut summary) => {
                if let Err(e) = promote(&staging, dest, options.overwrite) {
                    let _ = std::fs::remove_dir_all(&staging);
                    return Err(e);
                }
                summary.dest = dest.to_path_buf();
                Ok(summary)
            }
            Err(e) => {
                // Nothing is left behind on failure: a partial folder that
                // looks like a workspace is a trap.
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
            }
        }
    }

    fn download_into(
        &mut self,
        plan: &ImportPlan,
        staging: &Path,
        options: &ImportOptions,
        started: Instant,
    ) -> Result<ImportSummary, ImportError> {
        let total = plan.item_count();
        let mut index = 0usize;
        let mut failures: Vec<(String, String)> = Vec::new();
        let mut collections = 0usize;
        let mut environments = 0usize;

        // Where each workspace's files go. One workspace keeps the flat
        // `Collections/` + `Environments/` layout it has always had; several
        // each get a folder of their own, because merging two workspaces'
        // same-named collections into one directory would leave them told
        // apart only by a " (2)" suffix nobody can trace back to a workspace.
        let roots = workspace_roots(plan, staging);
        for (i, ws) in plan.workspaces.iter().enumerate() {
            if !ws.collections.is_empty() {
                std::fs::create_dir_all(roots[i].join(COLLECTIONS_DIR)).map_err(io_err)?;
            }
            if !ws.environments.is_empty() {
                std::fs::create_dir_all(roots[i].join(ENVIRONMENTS_DIR)).map_err(io_err)?;
            }
        }

        // De-duplication is per workspace, since each has its own folder: two
        // workspaces may both hold a "Billing API" without either being
        // renamed.
        let n = plan.workspaces.len();
        let mut taken_collections: Vec<HashSet<String>> = vec![HashSet::new(); n];
        let mut taken_environments: Vec<HashSet<String>> = vec![HashSet::new(); n];
        let mut notes: Vec<Vec<ConversionNote>> = vec![Vec::new(); n];

        // The queue, in the order the files must be written: one workspace at a
        // time, collections before environments. Fetching happens out of order
        // and in parallel; *processing* follows this order regardless, so the
        // names a workspace with two "Billing API"s produces don't depend on
        // which reply won the race.
        let mut jobs: Vec<(ItemKind, &ItemSummary)> = Vec::with_capacity(total);
        // Which workspace each job belongs to, and so which folder it lands in.
        let mut owners: Vec<usize> = Vec::with_capacity(total);
        for (i, ws) in plan.workspaces.iter().enumerate() {
            for item in &ws.collections {
                jobs.push((ItemKind::Collection, item));
                owners.push(i);
            }
            for item in &ws.environments {
                jobs.push((ItemKind::Environment, item));
                owners.push(i);
            }
        }

        self.fetch_each(&jobs, |job_index, fetched| {
            let (kind, item) = jobs[job_index];
            let ws = owners[job_index];
            {
                self.check_cancel()?;
                index += 1;
                let display = display_name(item, kind);
                self.send(ImportMsg::Item {
                    index,
                    total,
                    kind,
                    name: display.clone(),
                });

                let (body, rate) = match fetched {
                    Ok(v) => v,
                    Err(ImportError::Api(e)) if item_failure_is_survivable(&e) => {
                        // One missing or broken item must not cost the other
                        // fifty-nine.
                        let msg = e.to_string();
                        self.send(ImportMsg::ItemFailed {
                            name: display.clone(),
                            error: msg.clone(),
                        });
                        failures.push((display, msg));
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                self.observe(RateBucket::General, &rate);

                let (dir, taken, counter) = match kind {
                    ItemKind::Collection => (
                        COLLECTIONS_DIR,
                        &mut taken_collections[ws],
                        &mut collections,
                    ),
                    ItemKind::Environment => (
                        ENVIRONMENTS_DIR,
                        &mut taken_environments[ws],
                        &mut environments,
                    ),
                };
                let rendered = render(&display, &body, kind, options.format, taken);
                std::fs::write(roots[ws].join(dir).join(&rendered.name), rendered.contents)
                    .map_err(io_err)?;
                *counter += 1;

                // A collection's own variables become a `.vars` beside the
                // environments, which is where anything selectable as one
                // belongs — including when the workspace has no environments of
                // its own and the folder wouldn't otherwise exist.
                if let Some((stem, contents)) = rendered.vars {
                    let name = unique_file_name(&stem, "vars", &mut taken_environments[ws]);
                    let dir = roots[ws].join(ENVIRONMENTS_DIR);
                    std::fs::create_dir_all(&dir).map_err(io_err)?;
                    std::fs::write(dir.join(name), contents).map_err(io_err)?;
                }
                notes[ws].extend(rendered.notes);
            }
            Ok(())
        })?;

        for (i, ws) in plan.workspaces.iter().enumerate() {
            if let Some(report) = conversion_report(&ws.workspace_name, &notes[i]) {
                std::fs::write(roots[i].join(NOTES_FILE), report).map_err(io_err)?;
            }
        }

        // A workspace that could not even be listed is reported next to the
        // items that could not be fetched: both are "asked for, not imported",
        // and both are things the summary must not quietly drop.
        failures.extend(plan.skipped.iter().cloned());

        Ok(ImportSummary {
            dest: PathBuf::new(),
            workspace_name: plan.workspace_name().to_string(),
            collections,
            environments,
            failures,
            converted_with_notes: notes.iter().any(|n| !n.is_empty()),
            elapsed: self.clock.now().saturating_duration_since(started),
        })
    }

    /// Fetch every job, several at a time, handing the results to `on_result`
    /// **in job order** on this thread.
    ///
    /// Pacing alone never explained a slow import: the general limit allows a
    /// call every 200ms, but each one is a whole collection document fetched
    /// over a link that may have half a second of latency, so a sequential
    /// import spends nearly all of its time waiting for replies rather than for
    /// its own rate limit. Overlapping the round trips fills that dead time;
    /// the pacer still decides when each call may go out, so the budget is
    /// respected exactly as before — the calls just no longer queue behind each
    /// other's latency.
    ///
    /// Results are reordered before being handed on, so the files a workspace
    /// with two "Billing API"s produces don't depend on which reply won.
    fn fetch_each<F>(
        &self,
        jobs: &[(ItemKind, &ItemSummary)],
        mut on_result: F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(usize, Result<(String, RateInfo), ImportError>) -> Result<(), ImportError>,
    {
        if jobs.is_empty() {
            return Ok(());
        }
        let workers = self.concurrency.min(jobs.len()).max(1);
        let cursor = AtomicUsize::new(0);
        // Separate from the user's cancel flag: this only tells the workers to
        // stop taking new jobs, and must not read back as "the user cancelled".
        let stop = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            for _ in 0..workers {
                let tx = tx.clone();
                let cursor = &cursor;
                let stop = &stop;
                scope.spawn(move || {
                    loop {
                        if stop.load(Ordering::Relaxed) || self.cancelled() {
                            break;
                        }
                        let i = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some((kind, item)) = jobs.get(i).copied() else {
                            break;
                        };
                        self.pace(RateBucket::General);
                        let got = self.retrying(RateBucket::General, |c| match kind {
                            ItemKind::Collection => c.get_collection(item.fetch_id()),
                            ItemKind::Environment => c.get_environment(item.fetch_id()),
                        });
                        if tx.send((i, got)).is_err() {
                            break;
                        }
                    }
                });
            }
            // The last live sender, or the loop below would never end.
            drop(tx);

            let mut pending: HashMap<usize, Result<(String, RateInfo), ImportError>> =
                HashMap::new();
            let mut next = 0usize;
            let mut outcome = Ok(());
            for (i, got) in rx {
                pending.insert(i, got);
                while let Some(got) = pending.remove(&next) {
                    next += 1;
                    if let Err(e) = on_result(next - 1, got) {
                        outcome = Err(e);
                        break;
                    }
                }
                if outcome.is_err() {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            // A cancelled import leaves jobs unfetched; say so rather than
            // reporting a short but successful download.
            if outcome.is_ok() && next < jobs.len() {
                outcome = Err(ImportError::Cancelled);
            }
            outcome
        })
    }

    /// Run one API call, retrying the failures that a retry can fix.
    ///
    /// A 429 is not a failure so much as an instruction: wait the stated time
    /// and try again. The monthly cap is the exception — it will not clear, so
    /// it propagates immediately rather than burning three retries.
    fn retrying<T>(
        &self,
        bucket: RateBucket,
        mut call: impl FnMut(&PostmanClient) -> Result<T, ApiError>,
    ) -> Result<T, ImportError> {
        let mut attempt = 0;
        loop {
            self.check_cancel()?;
            match call(self.client) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    attempt += 1;
                    if !e.is_retryable() || attempt > MAX_RETRIES {
                        return Err(ImportError::Api(e));
                    }
                    let now = self.clock.now();
                    let wait = match &e {
                        ApiError::RateLimited { retry_after, .. } => {
                            let w = self.locked_pacer().back_off(bucket, *retry_after, now);
                            self.send(ImportMsg::Waiting {
                                reason: WaitReason::RateLimited,
                                secs: w.as_secs().max(1),
                            });
                            w
                        }
                        // A transport blip gets a short, growing pause rather
                        // than an immediate hammer on a server that may be
                        // struggling.
                        _ => Duration::from_millis(500) * attempt,
                    };
                    self.clock.sleep(wait);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The files one fetched item turns into, plus whatever converting it lost.
struct Rendered {
    /// The item's own file, for the directory its kind implies.
    name: String,
    contents: String,
    /// A `.vars` companion belonging in `Environments`, holding a collection's
    /// own `{{variable}}` defaults — they have nowhere to live in a `.hurl`,
    /// and without them the collection's URLs don't resolve.
    vars: Option<(String, String)>,
    notes: Vec<ConversionNote>,
}

/// Turn one fetched item into the files it should become.
///
/// This is the single place that decides an extension or rewrites a body, so
/// conversion is contained here and the download engine is unaware of it.
///
/// Conversion never costs data: if a collection doesn't convert — an export
/// shape we don't recognise, or one with no requests in it at all — the
/// original JSON is written instead and the reason is noted. PaperBoy opens
/// Postman JSON directly, so the fallback is still a usable collection.
fn render(
    display: &str,
    body: &str,
    kind: ItemKind,
    format: ImportFormat,
    taken: &mut HashSet<String>,
) -> Rendered {
    let stem = sanitize_file_name(display);
    let raw = |taken: &mut HashSet<String>| Rendered {
        name: unique_file_name(&stem, "json", taken),
        contents: body.to_string(),
        vars: None,
        notes: Vec::new(),
    };
    match (format, kind) {
        (ImportFormat::Raw, _) => raw(taken),
        (ImportFormat::Hurl, ItemKind::Collection) => {
            let converted = crate::postman::convert_postman(body);
            if converted.entries.is_empty() {
                let mut out = raw(taken);
                out.notes.push(ConversionNote {
                    item: display.to_string(),
                    detail: "no requests could be read, so the original Postman JSON was kept"
                        .to_string(),
                });
                return out;
            }
            let vars = (!converted.variables.is_empty()).then(|| {
                (
                    format!("{stem} (collection variables)"),
                    vars_text(&converted.variables),
                )
            });
            // Last line of defence: whatever comes out has to be a file
            // PaperBoy can open again. A single construct Hurl's parser
            // rejects — one `{{$guid}}`, one file part with no file — fails
            // the *whole* file, taking every other request in it with it, and
            // nothing about the folder on disk says why. Rather than write a
            // collection that cannot be read, keep the JSON, which always
            // opens, and say so.
            let contents = crate::hurl::collection_to_hurl(&converted.entries);
            // Reading back is the test, and recovery must not be allowed to
            // pass it: a request that comes back as unreadable text is exactly
            // the failure this guard exists to catch. An import should produce
            // requests, not something the user has to repair by hand.
            let read_back = crate::hurl::parse_hurl(&contents);
            if read_back.len() != converted.entries.len()
                || read_back.iter().any(|e| e.is_unreadable())
            {
                let mut out = raw(taken);
                out.notes.push(ConversionNote {
                    item: display.to_string(),
                    detail: format!(
                        "the converted Hurl did not read back correctly ({}), so the original \
                         Postman JSON was kept",
                        crate::hurl::parse_hurl_error(&contents)
                            .unwrap_or_else(|| "requests went missing".to_string())
                    ),
                });
                return out;
            }
            Rendered {
                name: unique_file_name(&stem, "hurl", taken),
                contents,
                vars,
                notes: converted.notes,
            }
        }
        (ImportFormat::Hurl, ItemKind::Environment) => {
            match crate::postman::postman_env_values(body) {
                Some(values) => Rendered {
                    name: unique_file_name(&stem, "vars", taken),
                    contents: vars_text(&values),
                    vars: None,
                    notes: Vec::new(),
                },
                None => {
                    let mut out = raw(taken);
                    out.notes.push(ConversionNote {
                        item: display.to_string(),
                        detail: "not a recognisable environment export, so the original Postman \
                                 JSON was kept"
                            .to_string(),
                    });
                    out
                }
            }
        }
    }
}

/// `KEY=value` lines — the `.vars` format, which is what both a converted
/// environment and a collection's own variables become.
fn vars_text(values: &[(String, String)]) -> String {
    let mut out = String::new();
    for (k, v) in values {
        // Enforce the line format here rather than trusting the caller to have
        // done it. A `.vars` file is one `KEY=value` per line: a newline in
        // either half invents a second variable (or a line the reader drops),
        // and an `=` in the *name* moves the split point, so `a=b` would read
        // back as `a` with the rest folded into its value. The converter
        // already cleans values on the way in; the writer that owns the format
        // shouldn't depend on that.
        let key: String = k
            .chars()
            .filter(|c| *c != '=' && *c != '\n' && *c != '\r')
            .collect();
        if key.trim().is_empty() {
            continue;
        }
        out.push_str(key.trim());
        out.push('=');
        out.push_str(&crate::environment::flatten_value(v));
        out.push('\n');
    }
    out
}

/// The `CONVERSION-NOTES.md` body, or `None` when nothing was lost — an empty
/// report is worse than no report, because a file that is always there stops
/// being read.
fn conversion_report(workspace: &str, notes: &[ConversionNote]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    // The workspace name is not always known — the headless import is given an
    // id, and paying a call on the strict rate-limit bucket just to title a
    // report would be a poor trade.
    let subject = match workspace.trim() {
        "" => "This folder".to_string(),
        name => format!("`{name}`"),
    };
    let mut out = format!(
        "# Conversion notes\n\n\
         {subject} was imported from Postman and converted to Hurl. Hurl does not\n\
         cover everything Postman does; this is what did not come across, so it can be\n\
         handled by hand rather than discovered at runtime.\n\n\
         Everything not listed here converted cleanly.\n"
    );
    let mut last = None;
    for note in notes {
        if last != Some(&note.item) {
            let heading = if note.item.trim().is_empty() {
                "(the collection itself)"
            } else {
                note.item.as_str()
            };
            out.push_str(&format!("\n## {heading}\n\n"));
            last = Some(&note.item);
        }
        out.push_str(&format!("- {}\n", note.detail));
    }
    Some(out)
}

fn display_name(item: &ItemSummary, kind: ItemKind) -> String {
    let trimmed = item.name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    // A nameless item still has to land somewhere findable; the id keeps two
    // of them from colliding meaninglessly.
    let fallback = match kind {
        ItemKind::Collection => "collection",
        ItemKind::Environment => "environment",
    };
    let id = item.fetch_id();
    if id.is_empty() {
        fallback.to_string()
    } else {
        format!("{fallback}-{id}")
    }
}

/// Whether a per-item error should be recorded and skipped rather than ending
/// the run. Anything that will recur for every remaining item — a rejected
/// key, the monthly cap — must stop the import instead of failing sixty times.
fn item_failure_is_survivable(e: &ApiError) -> bool {
    match e {
        ApiError::Unauthorized => false,
        ApiError::RateLimited { monthly, .. } => !monthly,
        _ => true,
    }
}

/// The staging folder each workspace's files are written under.
///
/// One workspace writes straight into the staging root, which keeps the layout
/// every existing import produced. Several get a folder each, named after the
/// workspace and de-duplicated the same way item files are, because two teams
/// really can both call a workspace "Platform".
fn workspace_roots(plan: &ImportPlan, staging: &Path) -> Vec<PathBuf> {
    if !plan.is_multi() {
        return vec![staging.to_path_buf(); plan.workspaces.len().max(1)];
    }
    let mut taken: HashSet<String> = HashSet::new();
    plan.workspaces
        .iter()
        .map(|ws| {
            let base = sanitize_file_name(&display_workspace(&ws.workspace_id, &ws.workspace_name));
            let mut candidate = base.clone();
            let mut n = 2;
            while !taken.insert(candidate.to_lowercase()) {
                candidate = format!("{base} ({n})");
                n += 1;
            }
            staging.join(candidate)
        })
        .collect()
}

/// Whether failing to *list* one workspace should drop it from a multi-
/// workspace plan rather than end the run. Only reasons that are specific to
/// that workspace qualify: a rejected key or an exhausted monthly budget would
/// fail for every other workspace too, and reporting forty identical skips
/// instead of one error would hide what actually went wrong.
fn workspace_failure_is_survivable(e: &ApiError) -> bool {
    matches!(e, ApiError::Forbidden(_) | ApiError::NotFound(_))
}

/// What to call a workspace in a message, falling back to the id when Postman
/// gave no name (the `--postman-workspace` path plans by id alone).
fn display_workspace(id: &str, name: &str) -> String {
    if name.trim().is_empty() {
        id.to_string()
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Workspace references
// ---------------------------------------------------------------------------

/// Pull a workspace id out of whatever the user supplied.
///
/// The id is a UUID, but nobody has one to hand — what people actually have is
/// the address bar of the workspace they are looking at. Postman writes those
/// as `https://go.postman.co/workspace/My-Team~<uuid>` or
/// `.../workspace/<uuid>/overview`, so a pasted URL is accepted as readily as
/// a bare id. Shared by the CLI and both wizards so all three take the same
/// input.
pub fn parse_workspace_ref(input: &str) -> Option<String> {
    let trimmed = input.trim().trim_matches('"').trim_matches('\'');
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_uuid(trimmed) {
        return Some(trimmed.to_ascii_lowercase());
    }

    // Walk the URL's segments (and the `name~uuid` form) looking for a UUID,
    // rather than assuming a fixed position, because Postman's paths vary by
    // view (`/overview`, `/request/…`, a query string, and so on).
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    without_query
        .split('/')
        .flat_map(|seg| seg.split('~'))
        .find(|seg| looks_like_uuid(seg))
        .map(|s| s.to_ascii_lowercase())
}

fn looks_like_uuid(s: &str) -> bool {
    // 8-4-4-4-12 hex. Deliberately not a full RFC check: the aim is to tell a
    // workspace id apart from a URL segment or a workspace name, and the
    // server is the authority on whether it exists.
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, g)| g.len() == *len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// Destination handling
// ---------------------------------------------------------------------------

fn io_err(e: std::io::Error) -> ImportError {
    ImportError::Io(e.to_string())
}

/// The staging folder, deliberately a *sibling* of the destination: a rename
/// is only atomic within one filesystem, and the system temp directory
/// routinely is not on the same one as the user's documents.
fn staging_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "import".to_string());
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.partial"))
}

fn ensure_destination(dest: &Path, overwrite: bool) -> Result<(), ImportError> {
    if !dest.exists() {
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        return Ok(());
    }
    if overwrite {
        return Ok(());
    }
    let empty = std::fs::read_dir(dest).map_err(io_err)?.next().is_none();
    if empty {
        Ok(())
    } else {
        Err(ImportError::DestNotEmpty(dest.to_path_buf()))
    }
}

/// Move the completed staging folder into place.
fn promote(staging: &Path, dest: &Path, overwrite: bool) -> Result<(), ImportError> {
    if dest.exists() {
        if !overwrite {
            // An empty directory cannot be the target of a rename on Unix, so
            // it is removed first; a non-empty one was already rejected.
            std::fs::remove_dir(dest).map_err(io_err)?;
        } else {
            // Only reached once the import has fully succeeded, so the window
            // in which the old copy is gone and the new one is not yet in
            // place is a single rename long.
            std::fs::remove_dir_all(dest).map_err(io_err)?;
        }
    }
    std::fs::rename(staging, dest).map_err(io_err)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postman_api::{HttpResponse, Transport};
    use std::cell::RefCell;
    use std::sync::Mutex;
    use std::sync::mpsc::channel;

    // -- virtual clock ----------------------------------------------------

    /// A clock that never really sleeps, so pacing behaviour can be asserted
    /// in microseconds instead of minutes.
    struct FakeClock {
        base: Instant,
        offset: Mutex<Duration>,
        slept: Mutex<Vec<Duration>>,
    }

    impl FakeClock {
        fn new() -> Self {
            FakeClock {
                base: Instant::now(),
                offset: Mutex::new(Duration::ZERO),
                slept: Mutex::new(Vec::new()),
            }
        }
        fn total_slept(&self) -> Duration {
            self.slept.lock().unwrap().iter().sum()
        }
        fn sleeps(&self) -> Vec<Duration> {
            self.slept.lock().unwrap().clone()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.base + *self.offset.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.offset.lock().unwrap() += d;
            self.slept.lock().unwrap().push(d);
        }
    }

    // -- fake transport ---------------------------------------------------

    struct Scripted {
        responses: Mutex<Vec<HttpResponse>>,
        urls: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
            Arc::new(Scripted {
                responses: Mutex::new(responses),
                urls: Mutex::new(Vec::new()),
            })
        }
    }

    struct Handle(Arc<Scripted>);

    impl Transport for Handle {
        fn get(&self, url: &str, _key: &str) -> Result<HttpResponse, String> {
            self.0.urls.lock().unwrap().push(url.to_string());
            let mut r = self.0.responses.lock().unwrap();
            if r.is_empty() {
                return Err("no scripted response left".into());
            }
            Ok(r.remove(0))
        }
    }

    fn res(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    fn res_with(status: u16, body: &str, headers: &[(&str, &str)]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_string(),
            // The real transport lowercases header names; the fake must too,
            // or these tests would pass on data the production path never sees.
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect(),
        }
    }

    fn client(script: Arc<Scripted>) -> PostmanClient {
        PostmanClient::with_transport("KEY".into(), None, Box::new(Handle(script)))
    }

    fn item(name: &str, uid: &str) -> ItemSummary {
        ItemSummary {
            uid: uid.into(),
            id: uid.into(),
            name: name.into(),
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        thread_local! {
            static N: RefCell<u32> = const { RefCell::new(0) };
        }
        let n = N.with(|c| {
            let mut c = c.borrow_mut();
            *c += 1;
            *c
        });
        let p = std::env::temp_dir().join(format!("pb-import-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    // -- pacer ------------------------------------------------------------

    /// A `RateInfo` carrying just the two numbers the pacer reads.
    fn budget(remaining: Option<u64>, reset_secs: Option<u64>) -> RateInfo {
        RateInfo {
            remaining,
            reset_secs,
            ..Default::default()
        }
    }

    /// The pacer's rules, grouped: every case was the same two-line setup and a
    /// single assertion about what the *next* `wait` costs, so they read far
    /// better against each other than one `#[test]` apiece.
    #[test]
    fn the_pacer_spaces_calls_per_bucket_and_follows_the_budget() {
        let clock = FakeClock::new();

        // The first call on a bucket is never delayed; the second pays the
        // bucket's interval.
        let mut p = Pacer::new();
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::ZERO);
        assert_eq!(p.wait(RateBucket::General, &clock), GENERAL_MIN_INTERVAL);

        // The whole point of two buckets: spending the slow one must not slow
        // the fast one down.
        let mut p = Pacer::new();
        p.wait(RateBucket::Strict, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::ZERO);

        // A healthy budget still never paces faster than the floor.
        let mut p = Pacer::new();
        p.observe(
            RateBucket::General,
            &budget(Some(300), Some(1)),
            clock.now(),
        );
        p.wait(RateBucket::General, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), GENERAL_MIN_INTERVAL);

        // An exhausted budget waits for the window to reset.
        let mut p = Pacer::new();
        p.observe(RateBucket::General, &budget(Some(0), Some(7)), clock.now());
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::from_secs(7));

        // Headers that don't carry both numbers say nothing, so the floor
        // stands.
        let mut p = Pacer::new();
        p.observe(RateBucket::General, &budget(Some(1), None), clock.now());
        p.wait(RateBucket::General, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), GENERAL_MIN_INTERVAL);

        // However long the server asks for, a wait is capped so the UI never
        // looks frozen.
        let mut p = Pacer::new();
        let forever = p.back_off(RateBucket::General, Some(86_400), clock.now());
        assert_eq!(forever, MAX_WAIT);
    }
    #[test]
    fn strict_bucket_is_five_times_slower_than_general() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        p.wait(RateBucket::Strict, &clock);
        assert_eq!(p.wait(RateBucket::Strict, &clock), STRICT_MIN_INTERVAL);
        assert!(STRICT_MIN_INTERVAL > GENERAL_MIN_INTERVAL);
    }

    #[test]
    fn time_already_spent_counts_towards_the_interval() {
        // A slow response should not be paid for twice.
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        p.wait(RateBucket::General, &clock);
        clock.sleep(GENERAL_MIN_INTERVAL);
        let before = clock.total_slept();
        let waited = p.wait(RateBucket::General, &clock);
        assert_eq!(waited, Duration::ZERO);
        assert_eq!(clock.total_slept(), before);
    }

    #[test]
    fn observing_a_tight_budget_slows_the_pace() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        // 10 calls left in 10 seconds is one per second, five times slower
        // than the general bucket's default.
        let info = RateInfo {
            remaining: Some(10),
            reset_secs: Some(10),
            ..Default::default()
        };
        p.observe(RateBucket::General, &info, clock.now());
        p.wait(RateBucket::General, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::from_secs(1));
    }

    #[test]
    fn back_off_without_a_server_hint_still_pauses() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        let waited = p.back_off(RateBucket::General, None, clock.now());
        assert!(waited >= Duration::from_secs(1));
    }

    // -- estimation -------------------------------------------------------

    fn plan_of(collections: usize, environments: usize) -> ImportPlan {
        ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: (0..collections)
                    .map(|i| item(&format!("c{i}"), &format!("uc{i}")))
                    .collect(),
                environments: (0..environments)
                    .map(|i| item(&format!("e{i}"), &format!("ue{i}")))
                    .collect(),
            }],
            Vec::new(),
            None,
        )
    }

    #[test]
    fn estimate_scales_with_the_item_count() {
        let plan = plan_of(60, 5);
        assert_eq!(plan.item_count(), 65);
        assert_eq!(
            plan.estimated_duration(),
            COLLECTION_FETCH_COST * 60 + ENVIRONMENT_FETCH_COST * 5
        );
    }

    /// What the pre-download estimate promises. Grouped: each case is one
    /// assertion about the same `estimated_duration`.
    #[test]
    fn the_estimate_reflects_what_the_download_actually_costs() {
        // A 500-environment workspace and a 500-collection one are not the
        // same download, and quoting one figure for both is how "about 2
        // minutes" became a quarter of an hour.
        assert!(plan_of(10, 0).estimated_duration() > plan_of(0, 10).estimated_duration());

        // Pacing is the floor, not the cost: an import that only counted the
        // interval between calls promised a time it could never hit.
        let plan = plan_of(60, 5);
        assert!(plan.estimated_duration() > GENERAL_MIN_INTERVAL * 65);

        assert_eq!(plan_of(0, 0).estimated_duration(), Duration::ZERO);
        assert!(!plan.strains_monthly_budget(), "silent without the header");
    }
    #[test]
    fn monthly_budget_warning_fires_when_the_import_is_a_big_share() {
        let mut plan = plan_of(60, 5);
        plan.remaining_month = Some(100);
        assert!(plan.strains_monthly_budget());
        plan.remaining_month = Some(100_000);
        assert!(!plan.strains_monthly_budget());
    }

    // -- planning ---------------------------------------------------------

    #[test]
    fn plan_lists_collections_and_environments() {
        let script = Scripted::new(vec![
            res(
                200,
                r#"{"collections":[{"uid":"u1","id":"1","name":"Alpha"}]}"#,
            ),
            res(
                200,
                r#"{"environments":[{"uid":"u2","id":"2","name":"Dev"}]}"#,
            ),
        ]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = imp
            .plan("ws-1", "My Workspace", &ImportOptions::default())
            .unwrap();
        assert_eq!(plan.collections.len(), 1);
        assert_eq!(plan.environments.len(), 1);
        assert_eq!(plan.workspace_name(), "My Workspace");
        let urls = script.urls.lock().unwrap();
        assert!(urls[0].contains("/collections?workspace=ws-1"));
        assert!(urls[1].contains("/environments?workspace=ws-1"));
    }

    #[test]
    fn an_unknown_workspace_reports_empty_rather_than_succeeding() {
        // Postman answers an unknown workspace id with 200 and an empty list,
        // so silence has to be turned into an error here or the user gets an
        // empty folder and no explanation.
        let script = Scripted::new(vec![
            res(200, r#"{"collections":[]}"#),
            res(200, r#"{"environments":[]}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let err = imp.plan("nope", "", &ImportOptions::default()).unwrap_err();
        assert_eq!(err, ImportError::Empty);
    }

    /// The migration case: several workspaces planned through one importer,
    /// so the pacer's picture of the account's budget survives the whole
    /// listing rather than being thrown away between workspaces.
    #[test]
    fn plan_all_covers_every_workspace_and_flattens_the_items() {
        let script = Scripted::new(vec![
            res(
                200,
                r#"{"collections":[{"uid":"u1","id":"1","name":"Alpha"}]}"#,
            ),
            res(
                200,
                r#"{"environments":[{"uid":"u2","id":"2","name":"Dev"}]}"#,
            ),
            res(
                200,
                r#"{"collections":[{"uid":"u3","id":"3","name":"Beta"}]}"#,
            ),
            res(200, r#"{"environments":[]}"#),
        ]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let targets = vec![
            ("ws-1".to_string(), "One".to_string()),
            ("ws-2".to_string(), "Two".to_string()),
        ];
        let plan = imp.plan_all(&targets, &ImportOptions::default()).unwrap();

        assert!(plan.is_multi());
        assert_eq!(plan.workspaces.len(), 2);
        assert_eq!(plan.item_count(), 3, "two collections and one environment");
        assert_eq!(
            plan.workspace_name(),
            "",
            "an import of several belongs to no one workspace"
        );
        let urls = script.urls.lock().unwrap();
        assert!(urls[0].contains("/collections?workspace=ws-1"));
        assert!(urls[2].contains("/collections?workspace=ws-2"));
    }

    /// An account nobody has tidied up has empty workspaces in it, and a key
    /// may have lost access to one. Neither may cost the other thirty-nine.
    #[test]
    fn plan_all_drops_the_empty_and_records_the_unreachable() {
        let script = Scripted::new(vec![
            res(200, r#"{"collections":[]}"#),
            res(200, r#"{"environments":[]}"#),
            res(403, r#"{"error":{"message":"not on your plan"}}"#),
            res(
                200,
                r#"{"collections":[{"uid":"u1","id":"1","name":"Alpha"}]}"#,
            ),
            res(200, r#"{"environments":[]}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let targets = vec![
            ("ws-empty".to_string(), "Empty".to_string()),
            ("ws-gone".to_string(), "Gone".to_string()),
            ("ws-ok".to_string(), "Keeper".to_string()),
        ];
        let plan = imp.plan_all(&targets, &ImportOptions::default()).unwrap();

        assert_eq!(plan.workspaces.len(), 1, "only the one with anything in it");
        assert_eq!(plan.workspaces[0].workspace_name, "Keeper");
        assert_eq!(plan.skipped.len(), 1, "the unreachable one is reported");
        assert_eq!(plan.skipped[0].0, "Gone");
    }

    /// A rejected key would fail for every remaining workspace, so it stops
    /// the plan rather than being logged forty times.
    #[test]
    fn plan_all_stops_on_a_failure_that_would_repeat() {
        let script = Scripted::new(vec![res(401, r#"{"error":{"message":"nope"}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let targets = vec![
            ("ws-1".to_string(), "One".to_string()),
            ("ws-2".to_string(), "Two".to_string()),
        ];
        let err = imp
            .plan_all(&targets, &ImportOptions::default())
            .unwrap_err();
        assert_eq!(err, ImportError::Api(ApiError::Unauthorized));
    }

    /// Several workspaces each get a folder; the same name in two of them is
    /// two files, not one renamed to " (2)".
    #[test]
    fn downloading_several_workspaces_gives_each_its_own_folder() {
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"n":1}}"#),
            res(200, r#"{"environment":{"name":"Dev"}}"#),
            res(200, r#"{"collection":{"n":2}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![
                WorkspacePlan {
                    workspace_id: "ws-1".into(),
                    workspace_name: "Alpha".into(),
                    collections: vec![item("Billing", "u1")],
                    environments: vec![item("Dev", "u2")],
                },
                WorkspacePlan {
                    workspace_id: "ws-2".into(),
                    workspace_name: "Beta".into(),
                    collections: vec![item("Billing", "u3")],
                    environments: vec![],
                },
            ],
            Vec::new(),
            None,
        );
        let dest = tmpdir("allws");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        assert_eq!(summary.collections, 2);
        assert_eq!(summary.environments, 1);
        assert!(dest.join("Alpha/Collections/Billing.json").is_file());
        assert!(dest.join("Alpha/Environments/Dev.json").is_file());
        assert!(dest.join("Beta/Collections/Billing.json").is_file());
        assert!(
            !dest.join("Beta/Collections/Billing (2).json").exists(),
            "de-duplication is per workspace, not across them"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    /// Two teams really can both call a workspace "Platform", and one folder
    /// holding both would lose half of it.
    #[test]
    fn workspaces_with_the_same_name_get_separate_folders() {
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"n":1}}"#),
            res(200, r#"{"collection":{"n":2}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![
                WorkspacePlan {
                    workspace_id: "ws-1".into(),
                    workspace_name: "Platform".into(),
                    collections: vec![item("A", "u1")],
                    environments: vec![],
                },
                WorkspacePlan {
                    workspace_id: "ws-2".into(),
                    workspace_name: "Platform".into(),
                    collections: vec![item("B", "u2")],
                    environments: vec![],
                },
            ],
            Vec::new(),
            None,
        );
        let dest = tmpdir("samename");
        imp.download(&plan, &dest, &ImportOptions::default())
            .unwrap();
        assert!(dest.join("Platform/Collections/A.json").is_file());
        assert!(dest.join("Platform (2)/Collections/B.json").is_file());
        std::fs::remove_dir_all(&dest).ok();
    }

    /// A workspace that could not be listed is not silently forgotten between
    /// the plan and the summary: the import reports it as not landed.
    #[test]
    fn skipped_workspaces_are_reported_in_the_summary() {
        let script = Scripted::new(vec![res(200, r#"{"collection":{"n":1}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws-1".into(),
                workspace_name: "Alpha".into(),
                collections: vec![item("A", "u1")],
                environments: vec![],
            }],
            vec![("Gone".to_string(), "not permitted".to_string())],
            None,
        );
        let dest = tmpdir("skipped");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();
        assert!(!summary.is_complete());
        assert_eq!(
            summary.failures,
            vec![("Gone".into(), "not permitted".into())]
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn plan_captures_the_monthly_budget_header() {
        let script = Scripted::new(vec![
            res_with(
                200,
                r#"{"collections":[{"uid":"u1","id":"1","name":"A"}]}"#,
                &[("RateLimit-Remaining-Month", "812")],
            ),
            res(200, r#"{"environments":[]}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = imp.plan("ws", "", &ImportOptions::default()).unwrap();
        assert_eq!(plan.remaining_month, Some(812));
    }

    #[test]
    fn plan_skips_the_calls_for_excluded_kinds() {
        let script = Scripted::new(vec![res(
            200,
            r#"{"collections":[{"uid":"u1","id":"1","name":"A"}]}"#,
        )]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let opts = ImportOptions {
            include_environments: false,
            ..Default::default()
        };
        let plan = imp.plan("ws", "", &opts).unwrap();
        assert!(plan.environments.is_empty());
        assert_eq!(script.urls.lock().unwrap().len(), 1);
    }

    // -- downloading ------------------------------------------------------

    #[test]
    fn download_writes_the_expected_folder_layout() {
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"info":{"name":"Alpha"}}}"#),
            res(200, r#"{"environment":{"name":"Dev"}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("Alpha", "u1")],
                environments: vec![item("Dev", "u2")],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("layout");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        assert_eq!(summary.collections, 1);
        assert_eq!(summary.environments, 1);
        assert!(summary.is_complete());
        assert!(dest.join("Collections/Alpha.json").is_file());
        assert!(dest.join("Environments/Dev.json").is_file());
        // The body must be stored exactly as received.
        let body = std::fs::read_to_string(dest.join("Collections/Alpha.json")).unwrap();
        assert_eq!(body, r#"{"collection":{"info":{"name":"Alpha"}}}"#);
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn duplicate_names_do_not_overwrite_each_other() {
        // Postman lets two collections share a name; the original script
        // silently lost one of them.
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"n":1}}"#),
            res(200, r#"{"collection":{"n":2}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("Same", "u1"), item("Same", "u2")],
                environments: vec![],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("dupes");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();
        assert_eq!(summary.collections, 2);
        let mut files: Vec<_> = std::fs::read_dir(dest.join("Collections"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        files.sort();
        assert_eq!(files.len(), 2);
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn one_missing_collection_does_not_abort_the_import() {
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"n":1}}"#),
            res(404, r#"{"error":{"message":"not found"}}"#),
            res(200, r#"{"collection":{"n":3}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let (tx, rx) = channel();
        let mut imp = Importer::new(&c).with_clock(&clock).with_progress(tx);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("A", "u1"), item("Gone", "u2"), item("C", "u3")],
                environments: vec![],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("survive");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        assert_eq!(summary.collections, 2);
        assert!(!summary.is_complete());
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].0, "Gone");
        assert!(dest.join("Collections/A.json").is_file());
        assert!(dest.join("Collections/C.json").is_file());
        assert!(!dest.join("Collections/Gone.json").exists());

        let msgs: Vec<_> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, ImportMsg::ItemFailed { .. }))
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn a_rejected_key_stops_the_whole_import() {
        // Unlike a missing collection, this will fail identically for every
        // item, so retrying the other fifty-nine is pure noise. Fetches overlap
        // now, so the calls already in flight when the first 401 lands go out
        // too — but nothing beyond them: the queue is abandoned, not worked
        // through.
        let script = Scripted::new(vec![res(401, "{}"), res(401, "{}"), res(401, "{}")]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock).with_concurrency(3);
        let plan = plan_of(3, 0);
        let dest = tmpdir("unauth");
        let err = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap_err();
        assert_eq!(err, ImportError::Api(ApiError::Unauthorized));
        assert!(
            script.urls.lock().unwrap().len() <= 3,
            "at most the calls already in flight, never one per item"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn the_monthly_cap_stops_the_import_without_retrying() {
        let monthly = || {
            res(
                429,
                r#"{"error":{"name":"serviceLimitExhausted","message":"monthly"}}"#,
            )
        };
        let script = Scripted::new(vec![monthly(), monthly(), monthly()]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock).with_concurrency(3);
        let plan = plan_of(3, 0);
        let dest = tmpdir("monthly");
        let err = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ImportError::Api(ApiError::RateLimited { monthly: true, .. })
        ));
        // No retries: waiting cannot clear a monthly cap. One call per item at
        // most — the three already in flight — and none of them waited out a
        // back-off, which is the only sleep long enough to show here (pacing
        // between concurrent fetches is milliseconds). A *range* rather than
        // exactly three: the first worker to see the cap stops the rest, so
        // how many of its peers had already sent their own request is a race.
        // What matters is that nobody asked twice.
        let calls = script.urls.lock().unwrap().len();
        assert!(
            (1..=3).contains(&calls),
            "one call per item at most: {calls}"
        );
        assert!(clock.total_slept() < Duration::from_secs(1));
    }

    #[test]
    fn a_429_is_waited_out_and_retried() {
        let script = Scripted::new(vec![
            res_with(
                429,
                r#"{"error":{"name":"rateLimited"}}"#,
                &[("Retry-After", "3")],
            ),
            res(200, r#"{"collection":{"n":1}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let (tx, rx) = channel();
        let mut imp = Importer::new(&c).with_clock(&clock).with_progress(tx);
        let plan = plan_of(1, 0);
        let dest = tmpdir("retry429");
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        assert_eq!(summary.collections, 1);
        assert!(clock.sleeps().contains(&Duration::from_secs(3)));
        let msgs: Vec<_> = rx.try_iter().collect();
        assert!(msgs.iter().any(|m| matches!(
            m,
            ImportMsg::Waiting {
                reason: WaitReason::RateLimited,
                secs: 3
            }
        )));
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn retries_are_bounded() {
        let script = Scripted::new(vec![
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
            res(429, r#"{"error":{"name":"rateLimited"}}"#),
        ]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = plan_of(1, 0);
        let dest = tmpdir("bounded");
        // A per-minute 429 is survivable, so the item is skipped rather than
        // the run dying — but only after a bounded number of attempts.
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();
        assert_eq!(summary.collections, 0);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(
            script.urls.lock().unwrap().len(),
            (MAX_RETRIES + 1) as usize
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn cancelling_stops_promptly_and_leaves_nothing_behind() {
        let script = Scripted::new(vec![res(200, r#"{"collection":{"n":1}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let cancel = Arc::new(AtomicBool::new(true));
        let mut imp = Importer::new(&c).with_clock(&clock).with_cancel(cancel);
        let plan = plan_of(5, 0);
        let dest = tmpdir("cancel");
        let err = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap_err();
        assert_eq!(err, ImportError::Cancelled);
        assert!(!dest.exists());
        assert!(!staging_path(&dest).exists());
    }

    /// A transport that answers slowly and remembers how many calls were in
    /// flight at once — the only way to tell an import that overlaps its
    /// fetches from one that merely looks fast.
    struct Slow {
        live: Mutex<usize>,
        peak: Mutex<usize>,
    }

    impl Transport for Arc<Slow> {
        fn get(&self, _url: &str, _key: &str) -> Result<HttpResponse, String> {
            {
                let mut live = self.live.lock().unwrap();
                *live += 1;
                let mut peak = self.peak.lock().unwrap();
                *peak = (*peak).max(*live);
            }
            std::thread::sleep(Duration::from_millis(40));
            *self.live.lock().unwrap() -= 1;
            Ok(res(200, r#"{"collection":{"n":1}}"#))
        }
    }

    /// Pacing was never the reason a big import crawled: the general limit
    /// allows five calls a second, but each one is a whole collection document
    /// fetched over a link that may have half a second of latency, so a
    /// sequential import sits waiting for replies. The fetches overlap now, and
    /// this is the test that says so — pinned to one at a time, the same
    /// download takes six times as long and never has two calls in flight.
    #[test]
    fn fetches_overlap_instead_of_queueing_behind_each_others_latency() {
        let slow = Arc::new(Slow {
            live: Mutex::new(0),
            peak: Mutex::new(0),
        });
        let c = PostmanClient::with_transport("k".into(), None, Box::new(slow.clone()));
        // A fake clock so the pacer's own waits cost no real time: what is
        // being measured here is the round trips, not the rate limit.
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c)
            .with_clock(&clock)
            .with_concurrency(FETCH_CONCURRENCY);
        let plan = plan_of(6, 0);
        let dest = tmpdir("overlap");
        let started = Instant::now();
        let summary = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(summary.collections, 6);
        assert!(
            *slow.peak.lock().unwrap() > 1,
            "the fetches never overlapped"
        );
        assert!(
            elapsed < Duration::from_millis(40 * 6),
            "six 40ms fetches took {elapsed:?} — that is one at a time"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn progress_messages_cover_every_item_in_order() {
        let script = Scripted::new(vec![
            res(200, r#"{"collection":{"n":1}}"#),
            res(200, r#"{"environment":{"n":2}}"#),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let (tx, rx) = channel();
        let mut imp = Importer::new(&c).with_clock(&clock).with_progress(tx);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("A", "u1")],
                environments: vec![item("E", "u2")],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("progress");
        imp.download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        let msgs: Vec<_> = rx.try_iter().collect();
        let items: Vec<_> = msgs
            .iter()
            .filter_map(|m| match m {
                ImportMsg::Item {
                    index, total, kind, ..
                } => Some((*index, *total, *kind)),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![(1, 2, ItemKind::Collection), (2, 2, ItemKind::Environment)]
        );
        assert!(msgs.iter().any(|m| matches!(m, ImportMsg::Done(_))));
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn every_failure_still_reports_an_outcome() {
        // A consumer waiting on the channel must always be released. An early
        // return that skipped the final message would hang the caller rather
        // than fail it — which is exactly what happened the first time the CLI
        // was pointed at an occupied folder.
        let dest = tmpdir("always-reports");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("mine.txt"), "x").unwrap();
        let script = Scripted::new(vec![]);
        let c = client(script);
        let clock = FakeClock::new();
        let (tx, rx) = channel();
        let mut imp = Importer::new(&c).with_clock(&clock).with_progress(tx);
        let _ = imp.download(&plan_of(1, 0), &dest, &ImportOptions::default());
        drop(imp);
        let msgs: Vec<_> = rx.iter().collect();
        assert!(
            msgs.iter().any(|m| matches!(m, ImportMsg::Failed(_))),
            "a failed import must announce itself, got {msgs:?}"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn a_successful_import_reports_done_exactly_once() {
        let script = Scripted::new(vec![res(200, r#"{"collection":{"n":1}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let (tx, rx) = channel();
        let mut imp = Importer::new(&c).with_clock(&clock).with_progress(tx);
        let dest = tmpdir("done-once");
        imp.download(&plan_of(1, 0), &dest, &ImportOptions::default())
            .unwrap();
        drop(imp);
        let dones = rx
            .iter()
            .filter(|m| matches!(m, ImportMsg::Done(_)))
            .count();
        assert_eq!(dones, 1);
        std::fs::remove_dir_all(&dest).ok();
    }

    // -- opening the result -----------------------------------------------

    #[test]
    fn an_imported_folder_opens_as_a_paperboy_workspace() {
        // The point of the whole feature. The engine and the workspace scanner
        // are separate modules, and a folder that downloads perfectly but does
        // not open is worth nothing, so the two halves are checked together.
        let script = Scripted::new(vec![
            res(
                200,
                r#"{"collection":{"info":{"name":"Billing","schema":"https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},"item":[{"name":"Get","request":{"method":"GET","url":"https://example.test/x"}}]}}"#,
            ),
            res(
                200,
                r#"{"environment":{"name":"Prod","values":[{"key":"BASE","value":"https://example.test","enabled":true}]}}"#,
            ),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("Billing", "u1")],
                environments: vec![item("Prod", "u2")],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("opens");
        imp.download(&plan, &dest, &ImportOptions::default())
            .unwrap();

        // The tree lists both files.
        let entries = crate::workspace::scan_workspace(&dest, true);
        let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
        assert_eq!(files.len(), 2, "both files should appear in the tree");

        // Each is classified as the right kind, not both as collections.
        let coll = dest.join(COLLECTIONS_DIR).join("Billing.json");
        let env = dest.join(ENVIRONMENTS_DIR).join("Prod.json");
        assert!(
            !crate::workspace::is_env_file(&coll),
            "a collection must not be taken for an environment"
        );
        assert!(
            crate::workspace::is_env_file(&env),
            "an imported Postman environment must be recognised as one"
        );

        // And each actually parses into the app's own model.
        let coll_text = std::fs::read_to_string(&coll).unwrap();
        assert!(crate::postman::looks_like_postman(&coll_text));
        assert_eq!(crate::postman::parse_collection(&coll_text).len(), 1);

        let env_text = std::fs::read_to_string(&env).unwrap();
        let values = crate::postman::postman_env_values(&env_text)
            .expect("the imported environment should parse");
        assert_eq!(
            values,
            vec![("BASE".to_string(), "https://example.test".to_string())]
        );

        std::fs::remove_dir_all(&dest).ok();
    }

    // -- destination safety ----------------------------------------------

    #[test]
    fn an_existing_non_empty_folder_is_refused() {
        let dest = tmpdir("occupied");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("mine.txt"), "precious").unwrap();
        let script = Scripted::new(vec![]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let err = imp
            .download(&plan_of(1, 0), &dest, &ImportOptions::default())
            .unwrap_err();
        assert_eq!(err, ImportError::DestNotEmpty(dest.clone()));
        // The user's file is untouched.
        assert_eq!(
            std::fs::read_to_string(dest.join("mine.txt")).unwrap(),
            "precious"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn overwrite_replaces_an_existing_folder() {
        let dest = tmpdir("overwrite");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.json"), "old").unwrap();
        let script = Scripted::new(vec![res(200, r#"{"collection":{"n":1}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let opts = ImportOptions {
            overwrite: true,
            ..Default::default()
        };
        imp.download(&plan_of(1, 0), &dest, &opts).unwrap();
        assert!(!dest.join("stale.json").exists());
        assert!(dest.join("Collections").is_dir());
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn a_failed_import_leaves_no_destination_at_all() {
        let script = Scripted::new(vec![res(401, "{}")]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("nothing");
        let _ = imp.download(&plan_of(2, 0), &dest, &ImportOptions::default());
        assert!(!dest.exists(), "no half-imported folder may survive");
        assert!(!staging_path(&dest).exists(), "no staging folder may leak");
    }

    #[test]
    fn staging_is_a_sibling_so_the_rename_stays_on_one_filesystem() {
        let dest = Path::new("/home/u/Documents/Backup");
        let staging = staging_path(dest);
        assert_eq!(staging.parent(), dest.parent());
        assert_ne!(staging, dest.to_path_buf());
    }

    #[test]
    fn a_stale_staging_folder_from_a_crashed_run_is_discarded() {
        let dest = tmpdir("stale-staging");
        let staging = staging_path(&dest);
        std::fs::create_dir_all(staging.join("Collections")).unwrap();
        std::fs::write(staging.join("Collections/Ghost.json"), "old run").unwrap();

        let script = Scripted::new(vec![res(200, r#"{"collection":{"n":1}}"#)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        imp.download(&plan_of(1, 0), &dest, &ImportOptions::default())
            .unwrap();
        assert!(!dest.join("Collections/Ghost.json").exists());
        std::fs::remove_dir_all(&dest).ok();
    }

    // -- workspace references ---------------------------------------------

    /// What counts as a workspace reference. Grouped: the accepted forms all
    /// normalise to the same id, and the rejected ones were already a table.
    #[test]
    fn a_workspace_reference_is_parsed_or_rejected() {
        let id = "12ece9e1-2abf-4edc-8e34-de66e74114d2";
        for (why, input) in [
            ("a bare id is taken as is", id.to_string()),
            (
                "surrounding quotes and space are tolerated",
                format!("  \"{id}\"  "),
            ),
            ("an uppercase id is normalised", id.to_uppercase()),
        ] {
            assert_eq!(parse_workspace_ref(&input), Some(id.to_string()), "{why}");
        }

        for bad in [
            "",
            "   ",
            "My Workspace",
            "https://go.postman.co/workspace/My-Team",
            "12ece9e1-2abf-4edc-8e34",
            "zzzzzzzz-2abf-4edc-8e34-de66e74114d2",
        ] {
            assert_eq!(parse_workspace_ref(bad), None, "should reject {bad:?}");
        }
    }
    #[test]
    fn a_pasted_workspace_url_yields_its_id() {
        // What a user actually has to hand is the address bar.
        for url in [
            "https://go.postman.co/workspace/My-Team~12ece9e1-2abf-4edc-8e34-de66e74114d2",
            "https://go.postman.co/workspace/12ece9e1-2abf-4edc-8e34-de66e74114d2/overview",
            "go.postman.co/workspace/Team~12ece9e1-2abf-4edc-8e34-de66e74114d2/request/123",
            "https://go.postman.co/workspace/12ece9e1-2abf-4edc-8e34-de66e74114d2?tab=x",
        ] {
            assert_eq!(
                parse_workspace_ref(url),
                Some("12ece9e1-2abf-4edc-8e34-de66e74114d2".to_string()),
                "failed on {url}"
            );
        }
    }

    // -- naming -----------------------------------------------------------

    #[test]
    fn a_nameless_item_still_gets_a_usable_file_name() {
        let it = ItemSummary {
            uid: "u9".into(),
            id: "9".into(),
            name: "   ".into(),
        };
        assert_eq!(display_name(&it, ItemKind::Collection), "collection-u9");
    }

    #[test]
    fn awkward_names_are_made_safe_on_disk() {
        let mut taken = HashSet::new();
        let r = render(
            "My API / v2: staging",
            "{}",
            ItemKind::Collection,
            ImportFormat::Raw,
            &mut taken,
        );
        assert!(!r.name.contains('/'));
        assert!(r.name.ends_with(".json"));
    }

    #[test]
    fn rendering_raw_never_alters_the_body() {
        let mut taken = HashSet::new();
        let body = r#"{"a":[1,2,3],"b":"\u00e9"}"#;
        let r = render(
            "x",
            body,
            ItemKind::Collection,
            ImportFormat::Raw,
            &mut taken,
        );
        assert_eq!(r.contents, body);
    }
    // -- Conversion to Hurl -------------------------------------------------

    fn hurl_options() -> ImportOptions {
        ImportOptions {
            format: ImportFormat::Hurl,
            ..Default::default()
        }
    }

    /// A collection with a folder, an inherited bearer token, a collection
    /// variable and a pre-request script — enough for every part of the
    /// conversion to show up in one import.
    const RICH_COLLECTION: &str = r#"{"collection":{
      "info": { "name": "API", "schema": "https://schema.getpostman.com/..v2.1.0" },
      "auth": { "type": "bearer", "bearer": [{ "key": "token", "value": "{{TOKEN}}" }] },
      "variable": [{ "key": "base", "value": "https://api.example.com" }],
      "item": [
        { "name": "Users", "item": [
          { "name": "List", "request": { "method": "GET", "url": "{{base}}/users" } }
        ]},
        { "name": "Legacy", "request": { "method": "GET", "url": "{{base}}/old" },
          "event": [{ "listen": "prerequest",
                      "script": { "exec": ["pm.environment.set('nonce', Date.now())"] } }] }
      ]}}"#;

    /// The point of the Hurl format: `.hurl` collections and `.vars`
    /// environments, with no Postman JSON left in the folder at all.
    #[test]
    fn converting_writes_hurl_collections_and_vars_environments() {
        let script = Scripted::new(vec![
            res(200, RICH_COLLECTION),
            res(
                200,
                r#"{"environment":{"name":"Prod","values":[{"key":"HOST","value":"x.example"}]}}"#,
            ),
        ]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = ImportPlan::new(
            vec![WorkspacePlan {
                workspace_id: "ws".into(),
                workspace_name: "WS".into(),
                collections: vec![item("API", "u1")],
                environments: vec![item("Prod", "u2")],
            }],
            Vec::new(),
            None,
        );
        let dest = tmpdir("convert");
        imp.download(&plan, &dest, &hurl_options()).unwrap();

        let hurl = std::fs::read_to_string(dest.join(COLLECTIONS_DIR).join("API.hurl")).unwrap();
        assert!(hurl.contains("GET {{base}}/users"), "{hurl}");
        assert!(
            hurl.contains("Users/List"),
            "the folder survives as a title prefix: {hurl}"
        );
        assert!(
            hurl.contains("Bearer {{TOKEN}}"),
            "the collection's auth was applied to the request that inherits it: {hurl}"
        );

        let env = std::fs::read_to_string(dest.join(ENVIRONMENTS_DIR).join("Prod.vars")).unwrap();
        assert_eq!(env, "HOST=x.example\n");

        // The collection's own variables have nowhere to live in a `.hurl`, so
        // they become an environment the user can actually select.
        let vars = std::fs::read_to_string(
            dest.join(ENVIRONMENTS_DIR)
                .join("API (collection variables).vars"),
        )
        .unwrap();
        assert_eq!(vars, "base=https://api.example.com\n");
        std::fs::remove_dir_all(&dest).ok();
    }

    /// The whole point of writing `.hurl` is that PaperBoy can open it again.
    /// A conversion that produced text its own parser rejects would look fine
    /// on disk and fail the moment anyone used it.
    #[test]
    fn the_converted_hurl_parses_back_into_the_same_requests() {
        let script = Scripted::new(vec![res(200, RICH_COLLECTION)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("roundtrip");
        imp.download(&plan_of(1, 0), &dest, &hurl_options())
            .unwrap();

        let text = std::fs::read_to_string(dest.join(COLLECTIONS_DIR).join("c0.hurl")).unwrap();
        let reparsed = crate::hurl::parse_hurl(&text);
        let direct = crate::postman::convert_postman(RICH_COLLECTION).entries;
        assert_eq!(
            reparsed.len(),
            direct.len(),
            "every request survives the round trip: {text}"
        );
        for (a, b) in reparsed.iter().zip(direct.iter()) {
            assert_eq!(a.title, b.title);
            assert_eq!(a.method, b.method);
            assert_eq!(a.url, b.url);
            assert_eq!(
                a.headers.len(),
                b.headers.len(),
                "the inherited auth header survives too, on {}",
                a.title
            );
        }
        std::fs::remove_dir_all(&dest).ok();
    }

    /// Converting is lossy, so what it lost is written down. A migration off
    /// Postman needs to know what still has to be done by hand.
    #[test]
    fn what_conversion_dropped_is_written_to_a_notes_file() {
        let script = Scripted::new(vec![res(200, RICH_COLLECTION)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("notes");
        let summary = imp
            .download(&plan_of(1, 0), &dest, &hurl_options())
            .unwrap();

        assert!(summary.converted_with_notes);
        let notes = std::fs::read_to_string(dest.join(NOTES_FILE)).unwrap();
        assert!(notes.contains("Legacy"), "the request is named: {notes}");
        assert!(
            notes.contains("pre-request script"),
            "and so is what it lost: {notes}"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    /// The failure this guard is for is silent: a converted file sits on disk
    /// looking like a collection and opens as nothing at all. If what came out
    /// doesn't read back, the JSON — which always opens — is kept instead, and
    /// the reason is written down.
    #[test]
    fn a_conversion_that_would_not_read_back_keeps_the_json() {
        let mut taken = HashSet::new();
        // An unclosed `{{` is a template Hurl cannot parse, and Postman is
        // perfectly happy to export one.
        let body = r#"{"info":{"name":"x","schema":"s"},"item":[{"name":"a","request":{
            "method":"POST","url":"https://x/{{unclosed"}}]}"#;
        let out = render(
            "x",
            body,
            ItemKind::Collection,
            ImportFormat::Hurl,
            &mut taken,
        );
        assert!(out.name.ends_with(".json"), "kept as JSON: {}", out.name);
        assert_eq!(out.contents, body, "byte for byte, so it always opens");
        assert!(!out.notes.is_empty(), "and the reason is recorded");
    }

    /// A clean conversion writes no notes file at all: a report that is always
    /// present is a report nobody reads.
    #[test]
    fn a_clean_conversion_leaves_no_notes_file() {
        let script = Scripted::new(vec![res(
            200,
            r#"{"collection":{"info":{"name":"A","schema":"x"},
                 "item":[{"name":"go","request":{"method":"GET","url":"https://x/y"}}]}}"#,
        )]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("clean");
        let summary = imp
            .download(&plan_of(1, 0), &dest, &hurl_options())
            .unwrap();

        assert!(!summary.converted_with_notes);
        assert!(!dest.join(NOTES_FILE).exists());
        std::fs::remove_dir_all(&dest).ok();
    }

    /// Conversion must never cost data. An export this build can't read falls
    /// back to the original JSON — which PaperBoy opens directly — rather than
    /// writing an empty `.hurl` and calling it done.
    #[test]
    fn a_collection_that_cannot_be_converted_keeps_its_original_json() {
        let body = r#"{"collection":{"info":{"name":"Odd","schema":"x"},"item":[]}}"#;
        let script = Scripted::new(vec![res(200, body)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("fallback");
        imp.download(&plan_of(1, 0), &dest, &hurl_options())
            .unwrap();

        let kept = std::fs::read_to_string(dest.join(COLLECTIONS_DIR).join("c0.json")).unwrap();
        assert_eq!(kept, body, "byte for byte, so nothing is lost");
        assert!(
            std::fs::read_to_string(dest.join(NOTES_FILE))
                .unwrap()
                .contains("original Postman JSON was kept"),
            "and the fallback is explained rather than silent"
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    /// The default format still writes Postman's JSON untouched, so choosing to
    /// convert stays an opt-in.
    #[test]
    fn the_default_format_still_writes_postman_json() {
        let script = Scripted::new(vec![res(200, RICH_COLLECTION)]);
        let c = client(script);
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let dest = tmpdir("raw-default");
        let summary = imp
            .download(&plan_of(1, 0), &dest, &ImportOptions::default())
            .unwrap();

        assert!(dest.join(COLLECTIONS_DIR).join("c0.json").is_file());
        assert!(!summary.converted_with_notes);
        assert!(!dest.join(NOTES_FILE).exists());
        std::fs::remove_dir_all(&dest).ok();
    }

    /// The `.vars` writer owns the line format, so it enforces it: one
    /// `KEY=value` per line. An `=` in a *name* moves the split point and a
    /// newline in either half invents a second variable — neither should
    /// depend on the converter having cleaned up first.
    #[test]
    fn the_vars_writer_keeps_one_variable_per_line() {
        let text = vars_text(&[
            ("a=b".to_string(), "one".to_string()),
            ("key".to_string(), "line one\nline two".to_string()),
            ("  ".to_string(), "dropped".to_string()),
        ]);
        assert_eq!(text.lines().count(), 2, "no line was invented: {text:?}");
        assert!(text.contains("ab=one"), "{text:?}");
        assert!(text.contains("key=line one line two"), "{text:?}");
    }
}
