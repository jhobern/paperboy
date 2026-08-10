//! Native OS file/folder pickers for the GUI.
//!
//! The terminal UI has its own in-app browser overlay (the right UX for a
//! terminal), so these helpers are GUI-only. They wrap `rfd`'s **synchronous**
//! `FileDialog` (its XDG-portal backend blocks on `pollster`, needing no system
//! GTK libraries).
//!
//! # Why the dialog runs on its own thread
//!
//! The synchronous call blocks until the user chooses a file. Called straight
//! from the egui update closure — as every one of these used to be — that
//! blocks the *whole* frame loop: the window stops repainting for as long as
//! the dialog is open, and the desktop eventually offers to force-quit the
//! "not responding" application. It also stalls every other per-frame poll,
//! so a report finishing while the user stood in a save dialog couldn't be
//! collected, and the export that followed reported nothing to export.
//!
//! So a picker is *requested*, not called: [`spawn`] runs the blocking dialog
//! on a worker thread and returns a [`PendingPick`] handle, which the update
//! loop polls with [`PendingPick::take`] once per frame and applies when it
//! resolves. This mirrors how a report run is already driven.
//!
//! This relies on the dialog being safe to open off the main thread, which
//! holds for the XDG-portal backend (it is D-Bus traffic, not a native toolkit
//! window). A macOS build would need `AsyncFileDialog` instead, as AppKit
//! panels are main-thread-only.
//!
//! Every picker is triggered by a button/menu click, never during a headless
//! test frame, so the tests never open a dialog.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Which dialog to open, together with everything it needs. Owned (rather than
/// borrowed) because it crosses to a worker thread.
pub enum PickKind {
    File {
        filters: Vec<(String, Vec<String>)>,
    },
    Folder,
    Save {
        default_name: String,
        filters: Vec<(String, Vec<String>)>,
    },
}

/// A dialog currently open on a worker thread.
///
/// `A` is the caller's own "what to do with this path" tag: the click that
/// opened the dialog knows which field or tab it was for, and by the time the
/// path arrives — many frames later — that context is long gone from the
/// stack, so it rides along here.
pub struct PendingPick<A> {
    rx: Receiver<Option<PathBuf>>,
    /// `Option` only so `take` can move it out on completion.
    action: Option<A>,
}

impl<A> PendingPick<A> {
    /// Poll the dialog. `None` means "still open" — the common case, since a
    /// user takes many frames to choose.
    ///
    /// Returns `Some((action, None))` on cancel rather than swallowing it: some
    /// callers have state to unwind when the user backs out.
    pub fn take(&mut self) -> Option<(A, Option<PathBuf>)> {
        match self.rx.try_recv() {
            Ok(path) => Some((self.action.take()?, path)),
            // The worker thread vanished without answering. Treat it exactly as
            // a cancel: a lost dialog must not wedge the picker slot shut.
            Err(TryRecvError::Disconnected) => Some((self.action.take()?, None)),
            Err(TryRecvError::Empty) => None,
        }
    }
}

/// Open a dialog on a worker thread, to be polled with [`PendingPick::take`].
pub fn spawn<A>(kind: PickKind, title: &str, dir: Option<&Path>, action: A) -> PendingPick<A> {
    let (tx, rx) = std::sync::mpsc::channel();
    let title = title.to_string();
    let dir = dir.map(Path::to_path_buf);
    std::thread::spawn(move || {
        let picked = match kind {
            PickKind::File { filters } => {
                with_owned_filters(base(&title, dir.as_deref()), &filters).pick_file()
            }
            PickKind::Folder => base(&title, dir.as_deref()).pick_folder(),
            PickKind::Save {
                default_name,
                filters,
            } => {
                let d = with_owned_filters(base(&title, dir.as_deref()), &filters);
                let d = if default_name.is_empty() {
                    d
                } else {
                    d.set_file_name(default_name)
                };
                d.save_file()
            }
        };
        // The receiver is gone if the window closed while the dialog was up;
        // there is nothing left to tell, so the error is genuinely ignorable.
        let _ = tx.send(picked);
    });
    PendingPick {
        rx,
        action: Some(action),
    }
}

/// Borrowed [`Filter`] rows in the owned form [`PickKind`] needs.
pub fn owned_filters(filters: &[Filter]) -> Vec<(String, Vec<String>)> {
    filters
        .iter()
        .map(|(n, e)| {
            (
                (*n).to_string(),
                e.iter().map(|x| (*x).to_string()).collect(),
            )
        })
        .collect()
}

fn with_owned_filters(
    mut d: rfd::FileDialog,
    filters: &[(String, Vec<String>)],
) -> rfd::FileDialog {
    for (name, exts) in filters {
        if exts.len() == 1 && exts[0] == "*" {
            continue; // "all files" — no restrictive filter
        }
        d = d.add_filter(name, exts);
    }
    d
}

/// A named group of extensions offered as a filter row in the picker
/// (`("Hurl / Postman", &["hurl", "json"])`). An entry of `&["*"]` is treated as
/// "all files" and adds no restrictive filter.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

fn base(title: &str, dir: Option<&Path>) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new().set_title(title);
    // Seed the starting directory from a sensible context (the last-used file's
    // folder, the collection's directory, …) when the caller has one.
    if let Some(dir) = dir.filter(|d| d.is_dir()) {
        d = d.set_directory(dir);
    }
    d
}

fn with_filters(mut d: rfd::FileDialog, filters: &[Filter]) -> rfd::FileDialog {
    for (name, exts) in filters {
        if *exts == ["*"] {
            continue; // "all files" — no restrictive filter
        }
        d = d.add_filter(*name, exts);
    }
    d
}

/// Open a native "pick a file" dialog, returning the chosen path (or `None` on
/// cancel). `filters` narrow the visible file types.
pub fn pick_file(title: &str, dir: Option<&Path>, filters: &[Filter]) -> Option<PathBuf> {
    with_filters(base(title, dir), filters).pick_file()
}

/// Open a native "pick a folder" dialog, returning the chosen directory.
pub fn pick_folder(title: &str, dir: Option<&Path>) -> Option<PathBuf> {
    base(title, dir).pick_folder()
}

/// Open a native "save file" dialog, returning the chosen path. `default_name`
/// pre-fills the filename field (e.g. `results.csv`).
pub fn save_file(
    title: &str,
    dir: Option<&Path>,
    default_name: &str,
    filters: &[Filter],
) -> Option<PathBuf> {
    let d = with_filters(base(title, dir), filters);
    let d = if default_name.is_empty() {
        d
    } else {
        d.set_file_name(default_name)
    };
    d.save_file()
}

/// Show a native error alert (used to report a failed open/save now that the
/// old in-app text dialog — which showed the error inline — is gone).
pub fn error_alert(title: &str, message: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .show();
}

/// The directory to seed a picker from, given an optional current path string.
/// Returns the file's parent (for a file path) or the path itself (for a
/// directory), so re-opening a picker lands where the user last was.
pub fn seed_dir(current: &str) -> Option<PathBuf> {
    if current.is_empty() {
        return None;
    }
    let p = PathBuf::from(current);
    if p.is_dir() {
        Some(p)
    } else {
        p.parent().map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a handle around a channel the test drives itself, standing in for
    /// the worker thread. Opening a real dialog in a test is not an option.
    fn pending<A>(action: A) -> (std::sync::mpsc::Sender<Option<PathBuf>>, PendingPick<A>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            tx,
            PendingPick {
                rx,
                action: Some(action),
            },
        )
    }

    /// The usual case by far: the user is still looking at the dialog, and the
    /// frame must carry on without them.
    #[test]
    fn an_open_dialog_reports_nothing_and_keeps_its_action() {
        let (_tx, mut p) = pending("open");
        assert!(p.take().is_none());
        assert!(p.take().is_none(), "and stays pollable");
    }

    #[test]
    fn a_chosen_path_arrives_with_the_action_that_asked_for_it() {
        let (tx, mut p) = pending("open");
        tx.send(Some(PathBuf::from("/tmp/a.hurl"))).unwrap();
        let (action, path) = p.take().expect("resolved");
        assert_eq!(action, "open");
        assert_eq!(path, Some(PathBuf::from("/tmp/a.hurl")));
    }

    /// A cancel is delivered, not swallowed: callers may have state to unwind.
    #[test]
    fn a_cancel_is_reported_as_a_resolved_dialog_with_no_path() {
        let (tx, mut p) = pending("save");
        tx.send(None).unwrap();
        let (action, path) = p.take().expect("resolved");
        assert_eq!(action, "save");
        assert_eq!(path, None);
    }

    /// If the worker thread dies without answering, the picker slot must still
    /// come unstuck -- otherwise the menu item is dead for the rest of the run.
    #[test]
    fn a_lost_dialog_resolves_like_a_cancel_rather_than_hanging() {
        let (tx, mut p) = pending("open");
        drop(tx);
        let (_, path) = p.take().expect("resolved rather than pending forever");
        assert_eq!(path, None);
    }
}
