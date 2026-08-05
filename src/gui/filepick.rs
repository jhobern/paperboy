//! Native OS file/folder pickers for the GUI.
//!
//! The terminal UI has its own in-app browser overlay (the right UX for a
//! terminal), so these helpers are GUI-only. They wrap `rfd`'s **synchronous**
//! `FileDialog` (its XDG-portal backend blocks on `pollster`, needing no system
//! GTK libraries), so a call opens a real native modal dialog and returns the
//! chosen path — or `None` when the user cancels. Every call is triggered by a
//! button/menu click, never during a headless test frame, so the tests never
//! block on a dialog.

use std::path::{Path, PathBuf};

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
