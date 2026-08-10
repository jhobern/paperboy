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
}

/// Per-run streaming bookkeeping: one [`RowState`] per skeleton row plus the
/// `path → row index` map that routes an out-of-order streamed row to its slot.
pub struct RunProgress {
    pub states: Vec<RowState>,
    pub index: HashMap<Vec<(usize, usize)>, usize>,
    pub done: usize,
    pub total: usize,
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
        if matches!(
            drain(handle, &mut self.result, &mut self.progress),
            Drained::Disconnected
        ) {
            self.run = None;
            return false;
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
}

impl RunHandle {
    /// Signal the worker to stop: subsequent requests short-circuit to a benign
    /// "cancelled" outcome so the flow winds down quickly (an in-flight request
    /// still finishes, but no new ones start). The partial grid is retained.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
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

    /// Whether the run has delivered its terminal `Done` (or the worker is gone).
    pub fn finished(&self) -> bool {
        self.finished
    }
}

impl Drop for RunHandle {
    /// Dropping the handle (editor closed, replaced, or navigated away) cancels
    /// the run so no detached worker keeps firing real HTTP requests. A run that
    /// already finished is unaffected (the flag is simply set on a dead worker).
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
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
        } = inputs;

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
                sink: None,
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
                    RowEvent::Completed(row) => RunUpdate::Row(Box::new(row.clone())),
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
            sink: Some(&sink),
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
    }
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
    loop {
        match handle.rx.try_recv() {
            Ok(update) => {
                if let Some(step) = apply(handle, update, result, progress) {
                    outcome = step;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                *progress = None;
                if !handle.finished {
                    return Drained::Disconnected;
                }
                break;
            }
        }
    }
    outcome
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
            let rows = finalized.rows.len();
            let errors = finalized.errors.len();
            *result = Some(finalized);
            Some(Drained::Done { rows, errors })
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
}
