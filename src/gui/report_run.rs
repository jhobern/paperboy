//! The GUI's report-run engine: run a PaperTrail flow on a background thread and
//! stream its rows back into a live results grid. This is the GUI-side, single-
//! run counterpart to the terminal UI's multi-tab run plumbing in
//! `tui/reports.rs` — it reuses the *same* front-end-agnostic executor
//! ([`crate::report::run`]) and run-input assembly ([`crate::report::context`]),
//! but stays deliberately small: the GUI editor owns at most one run at a time,
//! so there is no per-tab `report_id` routing.
//!
//! A run streams as: one [`RunUpdate::Skeleton`] (the full projected row set,
//! all cells still empty, from a no-HTTP dry expansion, so the grid appears
//! immediately greyed), then one [`RunUpdate::RowStarted`] / [`RunUpdate::Row`]
//! pair per iteration (routed to its slot by the row's structural `path`, which
//! is stable and unique even under out-of-order `PARALLEL` execution), then one
//! [`RunUpdate::Done`] with the finalized (comparison/baseline-collapsed)
//! result that replaces the streamed grid.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::hurl::{HurlEntry, RunOutput};
use crate::report::context::ReportRunInputs;
use crate::report::model::{ReportResult, ReportRow};
use crate::report::run::{
    DryRunner, EntryRunner, LiveRunner, RowEvent, RunContext, finalize, run_flow_raw,
};

/// A streaming update from a background report run, delivered over the run's
/// channel. Single-run, so unlike the TUI's `ReportRunUpdate` it carries no
/// `report_id` — the receiver *is* the run.
pub enum RunUpdate {
    /// The projected row set from a no-HTTP dry expansion: every row present but
    /// unfilled. Installed immediately as a greyed grid so the run's shape/size
    /// is visible before any request completes.
    Skeleton(ReportResult),
    /// A leaf row has begun running its requests (before any complete). Routed
    /// to its slot by `path` and drawn "running". Followed by a [`Self::Row`].
    RowStarted(Vec<(usize, usize)>),
    /// One completed iteration's row, matched into the skeleton by `path` and
    /// un-greyed. May arrive out of order under a `PARALLEL` loop.
    Row(Box<ReportRow>),
    /// The authoritative finalized result, replacing the streamed grid at the end.
    Done(ReportResult),
    /// A row re-run's finalized result, to be spliced into the grid's rows
    /// `rows` (see [`crate::report::rerun::splice`]) rather than replacing it.
    Rerun {
        rows: Vec<usize>,
        fresh: ReportResult,
    },
    /// A row re-run started while the run was still going: its fresh raw rows
    /// for the live grid, and the same rows finished, for the run's end.
    LiveRerun(Box<crate::report::rerun::LiveRerun>),
}

/// The live state of a row's slot in the streamed grid (drives its status icon
/// and greying), index-aligned with [`ReportResult::rows`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowState {
    /// In the skeleton but not started (its requests are queued) — drawn greyed.
    Scheduled,
    /// Its requests are in flight — drawn highlighted. Several at once under `PARALLEL`.
    Running,
    /// Its real result has streamed in — cells filled, drawn normally.
    Finished,
    /// Part of a finished result, with nothing happening to it: drawn
    /// normally, with nothing in the status column.
    Idle,
}

/// Per-run streaming bookkeeping: one [`RowState`] per skeleton row plus the
/// `path → row index` map that routes an out-of-order streamed row to its slot.
pub struct RunProgress {
    pub states: Vec<RowState>,
    pub index: HashMap<Vec<(usize, usize)>, usize>,
    pub done: usize,
    pub total: usize,
    /// Set while this is a row re-run over a finished result rather than a
    /// whole run: the grid stays a finished one (filters, metrics and sorting
    /// remain), and only the rows being re-run are drawn as running.
    pub rerun: bool,
}

impl RunProgress {
    /// The progress of re-running rows `rows` of a finished `total`-row grid.
    pub fn rerun(total: usize, rows: &[usize]) -> Self {
        let mut states = vec![RowState::Idle; total];
        for &r in rows {
            if let Some(s) = states.get_mut(r) {
                *s = RowState::Running;
            }
        }
        RunProgress {
            states,
            index: HashMap::new(),
            done: 0,
            total: rows.len(),
            rerun: true,
        }
    }
}

/// What identifies a report's run across the editor being closed and reopened.
///
/// A report opened from a Workspace tree is loaded afresh from disk each time,
/// so its [`Report::id`](crate::report::Report::id) is a *different* number on
/// the way back in — the file path is the only thing that survives. A session
/// report keeps its id (it is cloned out of the session), and may have no path
/// at all if it has never been saved.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RunKey {
    Path(std::path::PathBuf),
    Id(u64),
}

impl RunKey {
    pub fn of(report: &crate::report::Report) -> Self {
        match &report.path {
            Some(p) => RunKey::Path(p.clone()),
            None => RunKey::Id(report.id),
        }
    }
}

/// A report's run, parked while its editor isn't on screen.
///
/// The editor is a *view*: it is dropped and rebuilt whenever the user clicks a
/// tab or opens another file. A run must not be, because dropping its
/// [`RunHandle`] cancels the worker — so navigating away used to kill a report
/// mid-flight and throw away the rows it had already collected. The run lives on
/// the app instead, and the editor borrows it while it is open.
#[derive(Default)]
pub struct ParkedRun {
    pub result: Option<ReportResult>,
    pub progress: Option<RunProgress>,
    pub run: Option<RunHandle>,
    pub results_exported: bool,
    pub last_export: Option<String>,
    /// The answers typed into the run settings. They travel with the run
    /// because they are what produced it -- and because retyping them after a
    /// trip to another tab would be a poor reward for having answered once.
    pub params: crate::report::params::ParamValues,
    /// What the result was actually run with, and the flow it was run from —
    /// what re-running one of its rows has to reproduce.
    pub result_params: Vec<(String, String)>,
    pub result_flow: Option<crate::report::flow::ReportFlow>,
    /// The parameter values that flow was run with (see the editor's field).
    pub result_run_params: crate::report::params::ParamValues,
    /// Rows picked to re-run once the run in flight has finished.
    pub queued_reruns: Vec<crate::report::rerun::RowRef>,
    /// Which tab was open when the editor was put away, and whether it already
    /// had results to show at the time.
    ///
    /// A report is often opened to *read* rather than to run -- comparing the
    /// source of two of them means going back and forth -- and being returned
    /// to the results grid on every visit meant re-picking Source every time.
    /// The `had_result` half keeps the other behaviour intact: a run that
    /// finished while nobody was looking still opens on its rows, because the
    /// tab the user last chose was chosen before there was anything to see.
    pub view: Option<super::report_editor::EditorView>,
    pub had_result: bool,
}

impl ParkedRun {
    /// Whether this is worth keeping: a live run, or results someone might come
    /// back to.
    pub fn is_worth_keeping(&self) -> bool {
        self.run.is_some() || self.result.is_some()
    }

    /// Drain any buffered updates so a run that nobody is looking at still makes
    /// progress into its grid. Returns whether it is still live.
    pub fn pump(&mut self) -> bool {
        let Some(handle) = self.run.as_mut() else {
            return false;
        };
        match drain(handle, &mut self.result, &mut self.progress) {
            Drained::Disconnected => {
                self.run = None;
                return false;
            }
            // Rows changed while nobody was looking: whatever was exported
            // describes the table as it was.
            Drained::Rerun { replaced, .. } if !replaced.is_empty() => {
                self.results_exported = false;
                self.last_export = None;
            }
            _ => {}
        }
        if handle.finished() {
            self.run = None;
            return false;
        }
        true
    }
}

/// A handle on an in-flight run: the cancel flag (flip it to wind the run down)
/// and the receiver its updates stream over.
pub struct RunHandle {
    cancel: Arc<AtomicBool>,
    rx: Receiver<RunUpdate>,
    /// Set once a terminal [`RunUpdate::Done`] has been folded in, so the editor
    /// knows the run is over even before the sender disconnects.
    finished: bool,
    /// Rows re-run while the run was still going, each on a worker of its own
    /// (see [`RunHandle::rerun_live`]). Carried by the handle so they travel
    /// with the run when its editor is put away, and drained beside it.
    side: Vec<SideRerun>,
    /// Re-runs that landed while the run was going, finished, waiting for the
    /// run's own result: it was made from the rows they replaced, so each is
    /// spliced into it as it arrives, or it would put the old answers back.
    held: Vec<(Vec<crate::report::rerun::RowRef>, ReportResult)>,
}

/// One row re-run in flight beside a run.
struct SideRerun {
    cancel: Arc<AtomicBool>,
    rx: Receiver<RunUpdate>,
    /// The rows it re-runs, by identity: the grid they are in may be the
    /// finished one by the time the answer comes back.
    refs: Vec<crate::report::rerun::RowRef>,
    /// Grid rows drawn as running for it, put back when it ends.
    marked: Vec<usize>,
}

impl RunHandle {
    /// Signal the worker to stop: subsequent requests short-circuit to a benign
    /// "cancelled" outcome so the flow winds down quickly (an in-flight request
    /// still finishes, but no new ones start). The partial grid is retained.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        for side in &self.side {
            side.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Re-run the rows at `paths` straight away, beside the run in flight —
    /// someone who spots a wrong row mid-run wants its answer now, not at the
    /// end. `refs` names them for mapping onto the finished result; `marked`
    /// are the grid rows to draw as running meanwhile.
    pub fn rerun_live(
        &mut self,
        inputs: ReportRunInputs,
        paths: Vec<Vec<(usize, usize)>>,
        refs: Vec<crate::report::rerun::RowRef>,
        marked: Vec<usize>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_live_rerun(inputs, paths, cancel.clone(), tx);
        self.side.push(SideRerun {
            cancel,
            rx,
            refs,
            marked,
        });
    }

    /// Whether row re-runs started during the run are still out.
    #[cfg(test)]
    pub(crate) fn rerunning_live(&self) -> bool {
        !self.side.is_empty()
    }

    /// A side re-run wired to a caller-driven sender, for tests.
    #[cfg(test)]
    pub(crate) fn side_for_test(
        &mut self,
        refs: Vec<crate::report::rerun::RowRef>,
        marked: Vec<usize>,
    ) -> std::sync::mpsc::Sender<RunUpdate> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.side.push(SideRerun {
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            refs,
            marked,
        });
        tx
    }

    /// The shared cancel flag, so a test can watch it after the handle itself
    /// has been dropped (which is exactly what stopping a run does).
    #[cfg(test)]
    pub(crate) fn cancel_flag_for_test(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Whether this run has been cancelled.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Whether the run has delivered its terminal `Done` (or the worker is
    /// gone) and no row re-run started during it is still out.
    pub fn finished(&self) -> bool {
        self.finished && self.side.is_empty()
    }
}

impl Drop for RunHandle {
    /// Dropping the handle (editor closed, replaced, or navigated away) cancels
    /// the run so no detached worker keeps firing real HTTP requests. A run that
    /// already finished is unaffected (the flag is simply set on a dead worker).
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A handle wired to a caller-driven sender, so a test can push updates through
/// the real [`drain`]/[`apply`] fold without spawning a worker.
#[cfg(test)]
pub(crate) fn test_handle() -> (RunHandle, std::sync::mpsc::Sender<RunUpdate>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = RunHandle {
        cancel: Arc::new(AtomicBool::new(false)),
        rx,
        finished: false,
        side: Vec::new(),
        held: Vec::new(),
    };
    (handle, tx)
}

/// Wraps a real [`EntryRunner`] with a cancel flag so a running report can be
/// stopped mid-flight: once `cancel` flips, every subsequent request returns a
/// benign "cancelled" outcome instead of hitting the network. Mirrors the TUI's
/// `CancellableRunner`.
struct CancellableRunner<R: EntryRunner> {
    inner: R,
    cancel: Arc<AtomicBool>,
}

impl<R: EntryRunner> EntryRunner for CancellableRunner<R> {
    fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
        if self.cancel.load(Ordering::Relaxed) {
            return RunOutput {
                generated: Default::default(),
                entries: Vec::new(),
                error: Some("cancelled".to_string()),
            };
        }
        self.inner.run(base, vars)
    }
}

/// Spawn a background run of `inputs` against a real [`LiveRunner`], returning a
/// [`RunHandle`]. The worker streams a skeleton, then each row's start/finish,
/// then the finalized result (see [`RunUpdate`]).
pub fn spawn(inputs: ReportRunInputs) -> RunHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let ReportRunInputs {
            flow,
            entries,
            helpers,
            base_vars,
            named_envs,
            root,
            file_root,
            language,
            params,
        } = inputs;
        let strings = crate::i18n::Strings::mapped(&language, super::icons::drawable);

        // 1. Skeleton: expand with no HTTP to get the full canonical row set up
        //    front. Its rows map 1:1 (by `path`) to the live rows the sink will
        //    stream, so the grid can be pre-built and filled in place.
        let skeleton = {
            let dry_ctx = RunContext {
                entries: &entries,
                helpers: &helpers,
                base_vars: base_vars.clone(),
                named_envs: named_envs.clone(),
                root: root.clone(),
                runner: &DryRunner,
                strings: &strings,
                params: params.clone(),
                sink: None,
                shuffle: None,
                cancel: None,
            };
            run_flow_raw(&flow, &dry_ctx)
        };
        if tx.send(RunUpdate::Skeleton(skeleton)).is_err() {
            return; // Receiver gone (editor closed) — nothing more to do.
        }

        // 2. Live run: stream each row's lifecycle through a `Sync` sink (the
        //    `PARALLEL` workers call it from several threads, so the `Send`-only
        //    `mpsc::Sender` is wrapped in a `Mutex`).
        let runner = CancellableRunner {
            inner: LiveRunner { file_root },
            cancel: cancel_worker,
        };
        let row_tx = Mutex::new(tx.clone());
        let sink = move |ev: RowEvent| {
            if let Ok(tx) = row_tx.lock() {
                let msg = match ev {
                    RowEvent::Started(path) => RunUpdate::RowStarted(path.to_vec()),
                    RowEvent::Completed { row, .. } => RunUpdate::Row(Box::new(row.clone())),
                };
                let _ = tx.send(msg);
            }
        };
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &runner,
            strings: &strings,
            params: params.clone(),
            sink: Some(&sink),
            shuffle: None,
            cancel: None,
        };
        let mut result = run_flow_raw(&flow, &ctx);
        // 3. Finalize (comparison/baseline collapse) off the raw rows.
        finalize(&mut result, &flow, &ctx);
        let _ = tx.send(RunUpdate::Done(result));
    });
    RunHandle {
        cancel,
        rx,
        finished: false,
        side: Vec::new(),
        held: Vec::new(),
    }
}

/// Spawn a background re-run of rows `rows` (whose structural paths are
/// `paths`) of a finished run of `inputs`, returning a [`RunHandle`] that
/// delivers a single [`RunUpdate::Rerun`] to splice into the grid.
///
/// Nothing streams: the rows being re-run are already on screen, and a
/// comparison row's fresh value only exists once both of its sides have been
/// collapsed again, so there is no partial answer worth showing.
pub fn spawn_rerun(
    inputs: ReportRunInputs,
    paths: Vec<Vec<(usize, usize)>>,
    rows: Vec<usize>,
) -> RunHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let ReportRunInputs {
            flow,
            entries,
            helpers,
            base_vars,
            named_envs,
            root,
            file_root,
            language,
            params,
        } = inputs;
        let strings = crate::i18n::Strings::mapped(&language, super::icons::drawable);
        let runner = CancellableRunner {
            inner: LiveRunner { file_root },
            cancel: cancel_worker,
        };
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &runner,
            strings: &strings,
            params,
            sink: None,
            shuffle: None,
            cancel: None,
        };
        let fresh = crate::report::rerun::rerun_rows(&flow, &ctx, &paths);
        let _ = tx.send(RunUpdate::Rerun { rows, fresh });
    });
    RunHandle {
        cancel,
        rx,
        finished: false,
        side: Vec::new(),
        held: Vec::new(),
    }
}

/// Spawn the resumption of a stopped run: produce the rows still pending in
/// `prior` (the grid the stop left), streaming each into its slot exactly as a
/// run does, then deliver the whole grid finalized (see
/// [`crate::report::rerun::resume`]). No skeleton: the grid is already there.
pub fn spawn_resume(inputs: ReportRunInputs, prior: ReportResult) -> RunHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let ReportRunInputs {
            flow,
            entries,
            helpers,
            base_vars,
            named_envs,
            root,
            file_root,
            language,
            params,
        } = inputs;
        let strings = crate::i18n::Strings::mapped(&language, super::icons::drawable);
        let runner = CancellableRunner {
            inner: LiveRunner { file_root },
            cancel: cancel_worker,
        };
        let row_tx = Mutex::new(tx.clone());
        let sink = move |ev: RowEvent| {
            if let Ok(tx) = row_tx.lock() {
                let msg = match ev {
                    RowEvent::Started(path) => RunUpdate::RowStarted(path.to_vec()),
                    RowEvent::Completed { row, .. } => RunUpdate::Row(Box::new(row.clone())),
                };
                let _ = tx.send(msg);
            }
        };
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &runner,
            strings: &strings,
            params,
            sink: Some(&sink),
            shuffle: None,
            cancel: None,
        };
        let result = crate::report::rerun::resume(&flow, &ctx, prior);
        let _ = tx.send(RunUpdate::Done(result));
    });
    RunHandle {
        cancel,
        rx,
        finished: false,
        side: Vec::new(),
        held: Vec::new(),
    }
}

/// The worker for [`RunHandle::rerun_live`].
fn spawn_live_rerun(
    inputs: ReportRunInputs,
    paths: Vec<Vec<(usize, usize)>>,
    cancel: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<RunUpdate>,
) {
    // A test must not send real requests: the re-run resolves as a worker
    // that went away, which is a path worth exercising anyway.
    #[cfg(test)]
    {
        let _ = (inputs, paths, cancel, tx);
    }
    #[cfg(not(test))]
    thread::spawn(move || {
        let ReportRunInputs {
            flow,
            entries,
            helpers,
            base_vars,
            named_envs,
            root,
            file_root,
            language,
            params,
        } = inputs;
        let strings = crate::i18n::Strings::mapped(&language, super::icons::drawable);
        let runner = CancellableRunner {
            inner: LiveRunner { file_root },
            cancel,
        };
        let ctx = RunContext {
            entries: &entries,
            helpers: &helpers,
            base_vars,
            named_envs,
            root,
            runner: &runner,
            strings: &strings,
            params,
            sink: None,
            shuffle: None,
            cancel: None,
        };
        let fresh = crate::report::rerun::rerun_live(&flow, &ctx, &paths);
        let _ = tx.send(RunUpdate::LiveRerun(Box::new(fresh)));
    });
}

/// The outcome of draining a run's channel this frame, so the caller can update
/// its status line and decide whether to keep polling / repainting.
pub enum Drained {
    /// No updates this frame, run still live.
    Idle,
    /// Progress advanced: `done`/`total` rows finished.
    Progress { done: usize, total: usize },
    /// The run finished normally with this many rows / errors.
    Done { rows: usize, errors: usize },
    /// A row re-run finished: the rows it replaced (empty when it matched
    /// none, or was stopped) and how many errors those rows now carry.
    Rerun { replaced: Vec<usize>, errors: usize },
    /// A row re-run started mid-run landed in the live grid: the raw rows it
    /// replaced and how many errors they carry.
    LiveRerun { replaced: Vec<usize>, errors: usize },
    /// The worker disconnected without a terminal `Done` (panic / editor race).
    Disconnected,
}

/// Drain all buffered updates from `handle`, folding them into `result` /
/// `progress`. Returns what happened this frame. A cancelled run keeps its
/// partial grid: completed rows retain their real responses; unstarted rows stay
/// greyed. Mirrors the TUI's `apply_report_run_update` for the single-run case.
pub fn drain(
    handle: &mut RunHandle,
    result: &mut Option<ReportResult>,
    progress: &mut Option<RunProgress>,
) -> Drained {
    let mut outcome = Drained::Idle;
    // The run's end, if it came this call. It outranks whatever a side re-run
    // reports in the same call: the caller's end-of-run bookkeeping (resetting
    // a drill-down opened on the live grid's numbering, the run's status) must
    // not be skipped because a re-run happened to land beside it.
    let mut run_done: Option<Drained> = None;
    loop {
        match handle.rx.try_recv() {
            Ok(update) => {
                if let Some(step) = apply(handle, update, result, progress) {
                    if matches!(step, Drained::Done { .. }) {
                        run_done = Some(step);
                    } else {
                        outcome = step;
                    }
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if !handle.finished {
                    *progress = None;
                    return Drained::Disconnected;
                }
                break;
            }
        }
    }
    let mut i = 0;
    while i < handle.side.len() {
        match handle.side[i].rx.try_recv() {
            Ok(RunUpdate::LiveRerun(fresh)) => {
                let side = handle.side.remove(i);
                if let Some(step) = land_live_rerun(handle, side, *fresh, result, progress) {
                    outcome = step;
                }
            }
            Ok(_) | Err(TryRecvError::Empty) => i += 1,
            // The worker died without answering: put its rows back as they were.
            // Once the run is over its marks name live-grid rows, a numbering the
            // finished grid doesn't share, so the progress is rebuilt instead.
            Err(TryRecvError::Disconnected) => {
                let side = handle.side.remove(i);
                if handle.finished {
                    *progress = result
                        .as_ref()
                        .and_then(|res| progress_after_run(handle, res));
                } else {
                    unmark(&side, progress);
                }
            }
        }
    }
    run_done.unwrap_or(outcome)
}

/// Put the rows a side re-run drew as running back to settled.
fn unmark(side: &SideRerun, progress: &mut Option<RunProgress>) {
    if let Some(p) = progress.as_mut() {
        let settled = if p.rerun {
            RowState::Idle
        } else {
            RowState::Finished
        };
        for &r in &side.marked {
            if p.states.get(r) == Some(&RowState::Running) {
                p.states[r] = settled;
            }
        }
    }
}

/// The progress to show once the run is over but row re-runs started during
/// it are still out: those rows running, the rest settled.
fn progress_after_run(handle: &RunHandle, res: &ReportResult) -> Option<RunProgress> {
    if handle.side.is_empty() {
        return None;
    }
    let refs: Vec<_> = handle.side.iter().flat_map(|s| s.refs.clone()).collect();
    let rows = crate::report::rerun::rows_after_finalize(&refs, res);
    Some(RunProgress::rerun(res.rows.len(), &rows))
}

/// Fold a landed side re-run in: into the live grid while the run is going
/// (holding its finished rows for the run's end), straight into the finished
/// result once the run is over.
fn land_live_rerun(
    handle: &mut RunHandle,
    side: SideRerun,
    fresh: crate::report::rerun::LiveRerun,
    result: &mut Option<ReportResult>,
    progress: &mut Option<RunProgress>,
) -> Option<Drained> {
    if side.cancel.load(Ordering::Relaxed) {
        return None;
    }
    let res = result.as_mut()?;
    let errors_in = |res: &ReportResult, rows: &[usize]| -> usize {
        rows.iter()
            .map(|&r| res.rows[r].errors.len() + res.baseline_errors.get(&r).map_or(0, Vec::len))
            .sum()
    };
    if res.finalized {
        let rows = crate::report::rerun::rows_after_finalize(&side.refs, res);
        let replaced = crate::report::rerun::splice(res, &fresh.finished, &rows);
        let errors = errors_in(res, &replaced);
        *progress = progress_after_run(handle, res);
        return Some(Drained::Rerun { replaced, errors });
    }
    let replaced = crate::report::rerun::splice_live(res, &fresh.raw);
    let errors = errors_in(res, &replaced);
    unmark(&side, progress);
    handle.held.push((side.refs, fresh.finished));
    Some(Drained::LiveRerun { replaced, errors })
}

/// Fold one update into the grid state (see [`drain`]).
fn apply(
    handle: &mut RunHandle,
    update: RunUpdate,
    result: &mut Option<ReportResult>,
    progress: &mut Option<RunProgress>,
) -> Option<Drained> {
    // Ignore stray updates after a cancel, except the terminal `Done` (which we
    // let through so the run is marked finished and stops polling).
    let cancelled = handle.cancelled();
    match update {
        RunUpdate::Skeleton(skeleton) => {
            if cancelled {
                return None;
            }
            let mut skeleton = skeleton;
            let total = skeleton.rows.len();
            // Outstanding until each row streams in — see `ReportResult::pending`.
            skeleton.pending = (0..total).collect();
            let index = skeleton
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| (row.path.clone(), i))
                .collect();
            *result = Some(skeleton);
            *progress = Some(RunProgress {
                states: vec![RowState::Scheduled; total],
                index,
                done: 0,
                total,
                rerun: false,
            });
            Some(Drained::Progress { done: 0, total })
        }
        RunUpdate::RowStarted(path) => {
            if cancelled {
                return None;
            }
            if let Some(prog) = progress.as_mut()
                && let Some(&ri) = prog.index.get(&path)
                && prog.states.get(ri) == Some(&RowState::Scheduled)
            {
                prog.states[ri] = RowState::Running;
            }
            None
        }
        RunUpdate::Row(row) => {
            if cancelled {
                return None;
            }
            let (Some(res), Some(prog)) = (result.as_mut(), progress.as_mut()) else {
                return None;
            };
            if let Some(&ri) = prog.index.get(&row.path)
                && ri < res.rows.len()
            {
                res.rows[ri] = *row;
                res.pending.remove(&ri);
                if prog.states[ri] != RowState::Finished {
                    prog.states[ri] = RowState::Finished;
                    prog.done += 1;
                }
            }
            Some(Drained::Progress {
                done: prog.done,
                total: prog.total,
            })
        }
        RunUpdate::Done(finalized) => {
            handle.finished = true;
            *progress = None;
            if cancelled {
                // Keep the partial grid already in `result`.
                return Some(Drained::Done {
                    rows: result.as_ref().map(|r| r.rows.len()).unwrap_or(0),
                    errors: result.as_ref().map(|r| r.errors.len()).unwrap_or(0),
                });
            }
            let mut finalized = finalized;
            // The run's result was made from the rows re-run during it; put
            // their fresh answers back in.
            for (refs, fresh) in std::mem::take(&mut handle.held) {
                let rows = crate::report::rerun::rows_after_finalize(&refs, &finalized);
                crate::report::rerun::splice(&mut finalized, &fresh, &rows);
            }
            let rows = finalized.rows.len();
            let errors = finalized.errors.len();
            *progress = progress_after_run(handle, &finalized);
            *result = Some(finalized);
            Some(Drained::Done { rows, errors })
        }
        // Side re-runs answer on their own channels (see `drain`).
        RunUpdate::LiveRerun(_) => None,
        RunUpdate::Rerun { rows, fresh } => {
            handle.finished = true;
            *progress = None;
            if cancelled {
                return Some(Drained::Rerun {
                    replaced: Vec::new(),
                    errors: 0,
                });
            }
            let Some(res) = result.as_mut() else {
                return Some(Drained::Rerun {
                    replaced: Vec::new(),
                    errors: 0,
                });
            };
            let replaced = crate::report::rerun::splice(res, &fresh, &rows);
            let errors = replaced
                .iter()
                .map(|&r| {
                    res.rows[r].errors.len() + res.baseline_errors.get(&r).map_or(0, Vec::len)
                })
                .sum();
            Some(Drained::Rerun { replaced, errors })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::{ReportResult, ReportRow};

    fn row(path: Vec<(usize, usize)>, status: &str) -> ReportRow {
        let mut r = ReportRow::default();
        r.path = path;
        r.cells.insert("status".to_string(), status.to_string());
        r
    }

    fn skeleton(n: usize) -> ReportResult {
        let mut res = ReportResult::default();
        res.column_order = vec!["status".to_string()];
        res.rows = (0..n).map(|i| row(vec![(0, i)], "")).collect();
        res
    }

    #[test]
    fn streamed_rows_fill_their_slots_by_path_and_advance_progress() {
        let (mut handle, tx) = test_handle();
        let mut result = None;
        let mut progress = None;

        tx.send(RunUpdate::Skeleton(skeleton(3))).unwrap();
        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Progress { done: 0, total: 3 }
        ));
        assert_eq!(progress.as_ref().unwrap().states.len(), 3);
        assert!(
            progress
                .as_ref()
                .unwrap()
                .states
                .iter()
                .all(|s| *s == RowState::Scheduled)
        );

        // A row arriving out of order still lands in its slot (matched by path).
        tx.send(RunUpdate::RowStarted(vec![(0, 2)])).unwrap();
        tx.send(RunUpdate::Row(Box::new(row(vec![(0, 2)], "200"))))
            .unwrap();
        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Progress { done: 1, total: 3 }
        ));
        let res = result.as_ref().unwrap();
        assert_eq!(
            res.rows[2].cells.get("status").map(String::as_str),
            Some("200")
        );
        assert_eq!(progress.as_ref().unwrap().states[2], RowState::Finished);
        assert_eq!(progress.as_ref().unwrap().states[0], RowState::Scheduled);
    }

    #[test]
    fn done_replaces_the_grid_and_clears_progress() {
        let (mut handle, tx) = test_handle();
        let mut result = Some(skeleton(2));
        let mut progress = Some(RunProgress {
            states: vec![RowState::Scheduled; 2],
            index: Default::default(),
            done: 0,
            total: 2,
            rerun: false,
        });

        let mut finalized = ReportResult::default();
        finalized.column_order = vec!["status".to_string()];
        finalized.rows = vec![row(vec![], "OK")];
        tx.send(RunUpdate::Done(finalized)).unwrap();

        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Done { rows: 1, errors: 0 }
        ));
        assert!(progress.is_none());
        assert!(handle.finished());
        assert_eq!(result.as_ref().unwrap().rows.len(), 1);
    }

    #[test]
    fn a_cancelled_run_keeps_its_partial_grid_and_ignores_late_rows() {
        let (mut handle, tx) = test_handle();
        let mut result = None;
        let mut progress = None;

        tx.send(RunUpdate::Skeleton(skeleton(2))).unwrap();
        drain(&mut handle, &mut result, &mut progress);
        tx.send(RunUpdate::Row(Box::new(row(vec![(0, 0)], "200"))))
            .unwrap();
        drain(&mut handle, &mut result, &mut progress);

        // User stops the run.
        handle.cancel();
        // A late row after cancellation is ignored; the finished row is kept.
        tx.send(RunUpdate::Row(Box::new(row(vec![(0, 1)], "500"))))
            .unwrap();
        tx.send(RunUpdate::Done(ReportResult::default())).unwrap();
        drain(&mut handle, &mut result, &mut progress);

        let res = result.as_ref().unwrap();
        assert_eq!(
            res.rows[0].cells.get("status").map(String::as_str),
            Some("200")
        );
        // The second row never filled (cancelled before it streamed).
        assert_eq!(
            res.rows[1].cells.get("status").map(String::as_str),
            Some("")
        );
        assert!(handle.finished());
    }

    fn side(key: &str, role: crate::report::model::RowRole, path: usize, v: &str) -> ReportRow {
        ReportRow {
            key: vec![key.to_string()],
            role,
            comparison: Some("c".to_string()),
            path: vec![(0, 0), (1, path)],
            cells: [("v".to_string(), v.to_string())].into_iter().collect(),
            ..Default::default()
        }
    }

    /// A row re-run mid-run replaces both raw sides in the live grid at once,
    /// and its answer survives the run's own result, which was made from the
    /// rows it replaced.
    #[test]
    fn a_live_rerun_lands_now_and_outlives_the_runs_own_result() {
        use crate::report::model::RowRole::{Baseline, Candidate};
        use crate::report::rerun::{LiveRerun, RowRef};
        let (mut handle, main) = test_handle();
        let live = ReportResult {
            rows: vec![
                side("a", Baseline, 0, "old"),
                side("a", Candidate, 1, "old"),
            ],
            ..Default::default()
        };
        let mut result = Some(live.clone());
        let mut progress = Some(RunProgress {
            states: vec![RowState::Running, RowState::Running],
            index: Default::default(),
            done: 2,
            total: 2,
            rerun: false,
        });
        let tx = handle.side_for_test(vec![RowRef::of(&live.rows[1])], vec![0, 1]);
        assert!(!handle.finished(), "a re-run is out");

        let finished_row = side("a", Candidate, 1, "new");
        tx.send(RunUpdate::LiveRerun(Box::new(LiveRerun {
            raw: ReportResult {
                rows: vec![
                    side("a", Baseline, 0, "new"),
                    side("a", Candidate, 1, "new"),
                ],
                ..Default::default()
            },
            finished: ReportResult {
                rows: vec![finished_row],
                finalized: true,
                ..Default::default()
            },
        })))
        .unwrap();
        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::LiveRerun { ref replaced, .. } if replaced.len() == 2
        ));
        let res = result.as_ref().unwrap();
        assert!(res.rows.iter().all(|r| r.cells["v"] == "new"));
        assert_eq!(
            progress.as_ref().unwrap().states,
            vec![RowState::Finished, RowState::Finished]
        );

        // The run finishes with a result made from the old rows.
        main.send(RunUpdate::Done(ReportResult {
            rows: vec![side("a", Candidate, 1, "old")],
            finalized: true,
            ..Default::default()
        }))
        .unwrap();
        drain(&mut handle, &mut result, &mut progress);
        assert_eq!(result.as_ref().unwrap().rows[0].cells["v"], "new");
        assert!(handle.finished());
        assert!(progress.is_none());
    }

    /// A re-run that outlasts the run lands straight in the finished result,
    /// and the run counts as over only once it has.
    #[test]
    fn a_live_rerun_landing_after_the_run_goes_into_the_finished_result() {
        use crate::report::model::RowRole::{Baseline, Candidate};
        use crate::report::rerun::{LiveRerun, RowRef};
        let (mut handle, main) = test_handle();
        let live = ReportResult {
            rows: vec![
                side("a", Baseline, 0, "old"),
                side("a", Candidate, 1, "old"),
            ],
            ..Default::default()
        };
        let mut result = Some(live.clone());
        let mut progress = None;
        let tx = handle.side_for_test(vec![RowRef::of(&live.rows[0])], vec![0, 1]);
        main.send(RunUpdate::Done(ReportResult {
            rows: vec![side("a", Candidate, 1, "old")],
            finalized: true,
            ..Default::default()
        }))
        .unwrap();
        drain(&mut handle, &mut result, &mut progress);
        assert!(!handle.finished(), "the re-run is still out");
        assert_eq!(
            progress.as_ref().map(|p| p.states.clone()),
            Some(vec![RowState::Running]),
            "the baseline's re-run shows on the row it was folded into"
        );

        tx.send(RunUpdate::LiveRerun(Box::new(LiveRerun {
            raw: ReportResult::default(),
            finished: ReportResult {
                rows: vec![side("a", Candidate, 1, "new")],
                finalized: true,
                ..Default::default()
            },
        })))
        .unwrap();
        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Rerun { ref replaced, .. } if *replaced == vec![0]
        ));
        assert_eq!(result.as_ref().unwrap().rows[0].cells["v"], "new");
        assert!(handle.finished());
        assert!(progress.is_none());
    }

    /// The run's end and a side re-run read in the same drain: the drain
    /// reports the end (the caller has bookkeeping to do for it), and the
    /// re-run's answer still lands.
    #[test]
    fn a_runs_end_is_not_hidden_by_a_rerun_landing_beside_it() {
        use crate::report::model::RowRole::{Baseline, Candidate};
        use crate::report::rerun::{LiveRerun, RowRef};
        let (mut handle, main) = test_handle();
        let live = ReportResult {
            rows: vec![
                side("a", Baseline, 0, "old"),
                side("a", Candidate, 1, "old"),
            ],
            ..Default::default()
        };
        let mut result = Some(live.clone());
        let mut progress = None;
        let tx = handle.side_for_test(vec![RowRef::of(&live.rows[1])], vec![0, 1]);
        main.send(RunUpdate::Done(ReportResult {
            rows: vec![side("a", Candidate, 1, "old")],
            finalized: true,
            ..Default::default()
        }))
        .unwrap();
        tx.send(RunUpdate::LiveRerun(Box::new(LiveRerun {
            raw: ReportResult::default(),
            finished: ReportResult {
                rows: vec![side("a", Candidate, 1, "new")],
                finalized: true,
                ..Default::default()
            },
        })))
        .unwrap();
        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Done { .. }
        ));
        assert_eq!(result.as_ref().unwrap().rows[0].cells["v"], "new");
        assert!(handle.finished());
    }

    /// A row re-run's answer goes into the row it was asked for, and the grid
    /// around it is left as it was — rows, progress and all.
    #[test]
    fn a_rerun_splices_its_row_into_the_finished_grid() {
        let (mut handle, tx) = test_handle();
        let finished = ReportResult {
            column_order: vec!["status".to_string()],
            rows: vec![row(vec![(0, 0)], "200"), row(vec![(0, 1)], "503")],
            ..Default::default()
        };
        let mut result = Some(finished);
        let mut progress = Some(RunProgress::rerun(2, &[1]));
        assert_eq!(
            progress.as_ref().unwrap().states,
            vec![RowState::Idle, RowState::Running]
        );

        let fresh = ReportResult {
            rows: vec![row(vec![(0, 1)], "200")],
            ..Default::default()
        };
        tx.send(RunUpdate::Rerun {
            rows: vec![1],
            fresh,
        })
        .unwrap();

        assert!(matches!(
            drain(&mut handle, &mut result, &mut progress),
            Drained::Rerun { ref replaced, errors: 0 } if *replaced == vec![1]
        ));
        assert!(progress.is_none());
        assert!(handle.finished());
        let res = result.as_ref().unwrap();
        assert_eq!(res.rows.len(), 2);
        assert_eq!(
            res.rows[1].cells.get("status").map(String::as_str),
            Some("200")
        );
    }
}
