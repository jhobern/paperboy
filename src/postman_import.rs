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
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::postman_api::{
    ApiError, GENERAL_MIN_INTERVAL, ItemSummary, PostmanClient, RateInfo, STRICT_MIN_INTERVAL,
    sanitize_file_name, unique_file_name,
};

/// Subfolder for collections, matching the layout the original backup script
/// produced so an existing backup folder and a PaperBoy import look the same.
pub const COLLECTIONS_DIR: &str = "Collections";
/// Subfolder for environments.
pub const ENVIRONMENTS_DIR: &str = "Environments";

/// How many times a single item is retried before it is recorded as failed.
/// Kept low deliberately: with dozens of items, a long retry chain on each is
/// how an import turns into an apparent hang.
const MAX_RETRIES: u32 = 3;

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
    pub fn wait(&mut self, bucket: RateBucket, clock: &dyn Clock) -> Duration {
        let now = clock.now();
        let delay = self.delay_before(bucket, now);
        if !delay.is_zero() {
            clock.sleep(delay);
        }
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

/// What an import will fetch, worked out before any bulk downloading starts so
/// the user can be shown a cost and an ETA and given the chance to back out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    pub workspace_id: String,
    pub workspace_name: String,
    pub collections: Vec<ItemSummary>,
    pub environments: Vec<ItemSummary>,
    /// `RateLimit-Remaining-Month` as of the listing calls, when Postman sent
    /// it. Lets the confirmation step warn that an import would eat most of a
    /// user's monthly budget before it spends any of it.
    pub remaining_month: Option<u64>,
}

impl ImportPlan {
    /// Total number of items to download.
    pub fn item_count(&self) -> usize {
        self.collections.len() + self.environments.len()
    }

    /// API calls the download phase will make. The listing calls are already
    /// spent by the time a plan exists, so they are not counted.
    pub fn api_calls(&self) -> usize {
        self.item_count()
    }

    /// Roughly how long the download will take at the published rates.
    ///
    /// Every fetch is a general-bucket call, so this is simply the item count
    /// paced at the general interval. It is an estimate, not a promise: a
    /// throttled account will be slower, which is why the running import
    /// reports a measured ETA too (see [`Progress::eta`]).
    pub fn estimated_duration(&self) -> Duration {
        GENERAL_MIN_INTERVAL * self.item_count() as u32
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

/// Live progress, from which a measured ETA can be derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub elapsed: Duration,
}

impl Progress {
    /// Time remaining based on the rate actually achieved so far, which
    /// accounts for throttling the up-front estimate could not know about.
    /// `None` until there is enough data to extrapolate from.
    pub fn eta(&self) -> Option<Duration> {
        if self.done == 0 || self.done >= self.total {
            return None;
        }
        let per_item = self.elapsed / self.done as u32;
        Some(per_item * (self.total - self.done) as u32)
    }
}

// ---------------------------------------------------------------------------
// Options, messages, results
// ---------------------------------------------------------------------------

/// The on-disk form imported items take.
///
/// Only [`ImportFormat::Raw`] exists today. Converting to Hurl on the way in
/// is a separate change that needs `postman.rs` to grow collection-level
/// variable and auth handling first; it slots in at [`render_collection`]
/// without the engine changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportFormat {
    /// Postman's own JSON, byte for byte. PaperBoy opens `.json` collections
    /// and Postman environment exports directly, so this needs no conversion
    /// and cannot lose anything.
    #[default]
    Raw,
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
    pub elapsed: Duration,
}

impl ImportSummary {
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
    pacer: Pacer,
    clock: &'a dyn Clock,
    cancel: Arc<AtomicBool>,
    progress: Option<Sender<ImportMsg>>,
}

impl<'a> Importer<'a> {
    pub fn new(client: &'a PostmanClient) -> Self {
        Importer {
            client,
            pacer: Pacer::new(),
            clock: &RealClock,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }

    pub fn with_clock(mut self, clock: &'a dyn Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_progress(mut self, tx: Sender<ImportMsg>) -> Self {
        self.progress = Some(tx);
        self
    }

    fn send(&self, msg: ImportMsg) {
        if let Some(tx) = &self.progress {
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
    fn pace(&mut self, bucket: RateBucket) {
        let waited = self.pacer.wait(bucket, self.clock);
        if waited >= Duration::from_secs(1) {
            self.send(ImportMsg::Waiting {
                reason: WaitReason::Pacing,
                secs: waited.as_secs(),
            });
        }
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

        let collections = if options.include_collections {
            self.pace(RateBucket::Strict);
            let (items, rate) =
                self.retrying(RateBucket::Strict, |c| c.list_collections(workspace_id))?;
            remaining_month = rate.remaining_month.or(remaining_month);
            self.pacer
                .observe(RateBucket::Strict, &rate, self.clock.now());
            items
        } else {
            Vec::new()
        };

        self.check_cancel()?;

        let environments = if options.include_environments {
            self.pace(RateBucket::General);
            let (items, rate) =
                self.retrying(RateBucket::General, |c| c.list_environments(workspace_id))?;
            remaining_month = rate.remaining_month.or(remaining_month);
            self.pacer
                .observe(RateBucket::General, &rate, self.clock.now());
            items
        } else {
            Vec::new()
        };

        if collections.is_empty() && environments.is_empty() {
            return Err(ImportError::Empty);
        }

        let plan = ImportPlan {
            workspace_id: workspace_id.to_string(),
            workspace_name: workspace_name.to_string(),
            collections,
            environments,
            remaining_month,
        };
        self.send(ImportMsg::Planned(Box::new(plan.clone())));
        Ok(plan)
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
                self.send(ImportMsg::Done(Box::new(summary.clone())));
                Ok(summary)
            }
            Err(e) => {
                // Nothing is left behind on failure: a partial folder that
                // looks like a workspace is a trap.
                let _ = std::fs::remove_dir_all(&staging);
                self.send(ImportMsg::Failed(e.to_string()));
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

        if !plan.collections.is_empty() {
            std::fs::create_dir_all(staging.join(COLLECTIONS_DIR)).map_err(io_err)?;
        }
        if !plan.environments.is_empty() {
            std::fs::create_dir_all(staging.join(ENVIRONMENTS_DIR)).map_err(io_err)?;
        }

        let mut taken_collections: HashSet<String> = HashSet::new();
        let mut taken_environments: HashSet<String> = HashSet::new();

        for (kind, items) in [
            (ItemKind::Collection, &plan.collections),
            (ItemKind::Environment, &plan.environments),
        ] {
            for item in items {
                self.check_cancel()?;
                index += 1;
                let display = display_name(item, kind);
                self.send(ImportMsg::Item {
                    index,
                    total,
                    kind,
                    name: display.clone(),
                });

                self.pace(RateBucket::General);
                let fetched = self.retrying(RateBucket::General, |c| match kind {
                    ItemKind::Collection => c.get_collection(item.fetch_id()),
                    ItemKind::Environment => c.get_environment(item.fetch_id()),
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
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                self.pacer
                    .observe(RateBucket::General, &rate, self.clock.now());

                let (dir, taken, counter) = match kind {
                    ItemKind::Collection => {
                        (COLLECTIONS_DIR, &mut taken_collections, &mut collections)
                    }
                    ItemKind::Environment => {
                        (ENVIRONMENTS_DIR, &mut taken_environments, &mut environments)
                    }
                };
                let (file_name, contents) = render(&display, &body, options.format, taken);
                std::fs::write(staging.join(dir).join(&file_name), contents).map_err(io_err)?;
                *counter += 1;
            }
        }

        Ok(ImportSummary {
            dest: PathBuf::new(),
            workspace_name: plan.workspace_name.clone(),
            collections,
            environments,
            failures,
            elapsed: self.clock.now().saturating_duration_since(started),
        })
    }

    /// Run one API call, retrying the failures that a retry can fix.
    ///
    /// A 429 is not a failure so much as an instruction: wait the stated time
    /// and try again. The monthly cap is the exception — it will not clear, so
    /// it propagates immediately rather than burning three retries.
    fn retrying<T>(
        &mut self,
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
                            let w = self.pacer.back_off(bucket, *retry_after, now);
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

/// Turn one fetched item into a file name and contents.
///
/// Split out because this is the single point conversion to Hurl will hook
/// into: it is the only place that decides an extension or rewrites a body.
fn render(
    display: &str,
    body: &str,
    format: ImportFormat,
    taken: &mut HashSet<String>,
) -> (String, String) {
    match format {
        ImportFormat::Raw => {
            let name = unique_file_name(&sanitize_file_name(display), "json", taken);
            (name, body.to_string())
        }
    }
}

/// Reserved for the conversion phase; see [`ImportFormat`].
#[allow(dead_code)]
fn render_collection() {}

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

    #[test]
    fn first_call_on_a_bucket_is_not_delayed() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::ZERO);
    }

    #[test]
    fn second_call_waits_the_bucket_interval() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        p.wait(RateBucket::General, &clock);
        let waited = p.wait(RateBucket::General, &clock);
        assert_eq!(waited, GENERAL_MIN_INTERVAL);
    }

    #[test]
    fn buckets_are_paced_independently() {
        // The whole point of two buckets: spending the slow one must not slow
        // the fast one down.
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        p.wait(RateBucket::Strict, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::ZERO);
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
    fn observing_a_healthy_budget_does_not_speed_past_the_floor() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        let info = RateInfo {
            remaining: Some(300),
            reset_secs: Some(1),
            ..Default::default()
        };
        p.observe(RateBucket::General, &info, clock.now());
        p.wait(RateBucket::General, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), GENERAL_MIN_INTERVAL);
    }

    #[test]
    fn an_exhausted_budget_waits_for_the_window_to_reset() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        let info = RateInfo {
            remaining: Some(0),
            reset_secs: Some(7),
            ..Default::default()
        };
        p.observe(RateBucket::General, &info, clock.now());
        assert_eq!(p.wait(RateBucket::General, &clock), Duration::from_secs(7));
    }

    #[test]
    fn observe_ignores_incomplete_headers() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        let info = RateInfo {
            remaining: Some(1),
            reset_secs: None,
            ..Default::default()
        };
        p.observe(RateBucket::General, &info, clock.now());
        p.wait(RateBucket::General, &clock);
        assert_eq!(p.wait(RateBucket::General, &clock), GENERAL_MIN_INTERVAL);
    }

    #[test]
    fn a_wait_is_capped_so_the_ui_never_looks_frozen() {
        let clock = FakeClock::new();
        let mut p = Pacer::new();
        let waited = p.back_off(RateBucket::General, Some(86_400), clock.now());
        assert_eq!(waited, MAX_WAIT);
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
        ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "WS".into(),
            collections: (0..collections)
                .map(|i| item(&format!("c{i}"), &format!("uc{i}")))
                .collect(),
            environments: (0..environments)
                .map(|i| item(&format!("e{i}"), &format!("ue{i}")))
                .collect(),
            remaining_month: None,
        }
    }

    #[test]
    fn estimate_scales_with_the_item_count() {
        let plan = plan_of(60, 5);
        assert_eq!(plan.item_count(), 65);
        assert_eq!(plan.estimated_duration(), GENERAL_MIN_INTERVAL * 65);
    }

    #[test]
    fn estimate_beats_the_uniform_one_second_approach() {
        // The regression this whole two-bucket design exists to prevent: the
        // original script paced every call at the strict rate.
        let plan = plan_of(60, 5);
        let naive = Duration::from_millis(1100) * 65;
        assert!(plan.estimated_duration() * 4 < naive);
    }

    #[test]
    fn an_empty_plan_estimates_nothing() {
        assert_eq!(plan_of(0, 0).estimated_duration(), Duration::ZERO);
    }

    #[test]
    fn monthly_budget_warning_fires_when_the_import_is_a_big_share() {
        let mut plan = plan_of(60, 5);
        plan.remaining_month = Some(100);
        assert!(plan.strains_monthly_budget());
        plan.remaining_month = Some(100_000);
        assert!(!plan.strains_monthly_budget());
    }

    #[test]
    fn monthly_budget_warning_is_silent_without_the_header() {
        let plan = plan_of(60, 5);
        assert!(!plan.strains_monthly_budget());
    }

    #[test]
    fn measured_eta_extrapolates_from_progress() {
        let p = Progress {
            done: 10,
            total: 50,
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(p.eta(), Some(Duration::from_secs(40)));
    }

    #[test]
    fn measured_eta_is_unavailable_before_and_after() {
        let none_yet = Progress {
            done: 0,
            total: 5,
            elapsed: Duration::from_secs(1),
        };
        let all_done = Progress {
            done: 5,
            total: 5,
            elapsed: Duration::from_secs(1),
        };
        assert_eq!(none_yet.eta(), None);
        assert_eq!(all_done.eta(), None);
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
        assert_eq!(plan.workspace_name, "My Workspace");
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
        let plan = ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "WS".into(),
            collections: vec![item("Alpha", "u1")],
            environments: vec![item("Dev", "u2")],
            remaining_month: None,
        };
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
        let plan = ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "WS".into(),
            collections: vec![item("Same", "u1"), item("Same", "u2")],
            environments: vec![],
            remaining_month: None,
        };
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
        let plan = ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "WS".into(),
            collections: vec![item("A", "u1"), item("Gone", "u2"), item("C", "u3")],
            environments: vec![],
            remaining_month: None,
        };
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
        // remaining item, so retrying the other fifty-nine is pure noise.
        let script = Scripted::new(vec![res(401, "{}")]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = plan_of(3, 0);
        let dest = tmpdir("unauth");
        let err = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap_err();
        assert_eq!(err, ImportError::Api(ApiError::Unauthorized));
        assert_eq!(script.urls.lock().unwrap().len(), 1);
        assert!(!dest.exists());
    }

    #[test]
    fn the_monthly_cap_stops_the_import_without_retrying() {
        let script = Scripted::new(vec![res(
            429,
            r#"{"error":{"name":"serviceLimitExhausted","message":"monthly"}}"#,
        )]);
        let c = client(script.clone());
        let clock = FakeClock::new();
        let mut imp = Importer::new(&c).with_clock(&clock);
        let plan = plan_of(3, 0);
        let dest = tmpdir("monthly");
        let err = imp
            .download(&plan, &dest, &ImportOptions::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ImportError::Api(ApiError::RateLimited { monthly: true, .. })
        ));
        // No retries: waiting cannot clear a monthly cap.
        assert_eq!(script.urls.lock().unwrap().len(), 1);
        assert_eq!(clock.total_slept(), Duration::ZERO);
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
        let plan = ImportPlan {
            workspace_id: "ws".into(),
            workspace_name: "WS".into(),
            collections: vec![item("A", "u1")],
            environments: vec![item("E", "u2")],
            remaining_month: None,
        };
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
        let (name, _) = render("My API / v2: staging", "{}", ImportFormat::Raw, &mut taken);
        assert!(!name.contains('/'));
        assert!(name.ends_with(".json"));
    }

    #[test]
    fn rendering_raw_never_alters_the_body() {
        let mut taken = HashSet::new();
        let body = r#"{"a":[1,2,3],"b":"\u00e9"}"#;
        let (_, out) = render("x", body, ImportFormat::Raw, &mut taken);
        assert_eq!(out, body);
    }
}
