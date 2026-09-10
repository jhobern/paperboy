//! Folder-structure support for collections. Neither Hurl nor our own
//! `HurlEntry` model has a real notion of folders, but a request's `title`
//! can encode one by using `/` as a path separator (e.g. `"Auth/Login"`,
//! `"Auth/Tokens/Refresh"`) — the same convention Postman collections use for
//! nested folders once imported (see [`crate::postman::import_postman`]).
//! This module turns that convention into an expand/collapse tree: folders
//! appear where the file first mentions them, and a folder's contents are
//! listed under it, indented, when it is expanded. Any number of folders can
//! be open at once -- the earlier model showed one folder at a time and made
//! comparing two of them a matter of walking in and out of each.

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::hurl::HurlEntry;

/// How the Requests list orders its rows.
///
/// View state, not a property of the collection: sorting changes what the list
/// looks like, never what the file says or what Run All executes. Cycled by the
/// GUI's sort button; the terminal UI leaves it at the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub enum SortMode {
    /// The order the file lists its requests in. The default, because a
    /// `.hurl` collection is a sequence and that sequence is what runs.
    #[default]
    File,
    /// By display name, A-Z.
    Alpha,
    /// By display name, Z-A.
    ReverseAlpha,
}

impl SortMode {
    /// The next mode the sort button steps to, wrapping back to `File` so the
    /// file's own order is always at most two clicks away.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn next(self) -> Self {
        match self {
            Self::File => Self::Alpha,
            Self::Alpha => Self::ReverseAlpha,
            Self::ReverseAlpha => Self::File,
        }
    }
}

/// The name a request shows in the list: the leaf of its `/`-encoded title,
/// since the folder rows above it supply the rest.
pub fn leaf_name(entry: &HurlEntry) -> String {
    entry_path(&entry.title).pop().unwrap_or_default()
}

/// Order two display names under `mode`, case-insensitively.
///
/// `File` reports every pair equal, which is what makes a *stable* sort under
/// it a no-op — so callers need no special case, and switching back to `File`
/// restores the file's order rather than some previous sort's leftovers.
pub fn cmp_names(mode: SortMode, a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    match mode {
        SortMode::File => Ordering::Equal,
        SortMode::Alpha => a.cmp(&b),
        SortMode::ReverseAlpha => b.cmp(&a),
    }
}

/// Reorder a **flat** list of rows in place under `mode`.
///
/// Only ever applied to a filtered list, which has no folder rows and no
/// nesting: sorting a tree by name as one flat sequence would tear children
/// away from their folders. The GUI sorts its unfiltered tree level by level
/// in its own builder, and the terminal UI leaves the order the file's.
pub fn sort_rows(rows: &mut [Row], entries: &[HurlEntry], mode: SortMode) {
    let name = |row: &Row| match row {
        Row::Folder { path, .. } => path.last().cloned().unwrap_or_default(),
        Row::Entry(i) => entries.get(*i).map(leaf_name).unwrap_or_default(),
    };
    rows.sort_by(|a, b| cmp_names(mode, &name(a), &name(b)));
}

/// Split a request title into its folder path, e.g. `"Auth/Login"` →
/// `["Auth", "Login"]`. Always returns at least one element (the leaf name,
/// which may be an empty string for an untitled request), so every entry
/// belongs somewhere — untitled/unnested requests simply live at the root.
pub fn entry_path(title: &str) -> Vec<String> {
    let segs: Vec<String> = title
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if segs.is_empty() {
        vec![String::new()]
    } else {
        segs
    }
}

/// One row in the folder-aware requests list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A folder, given by its full path from the root, and whether it is
    /// currently expanded.
    ///
    /// The whole path rather than just the name: two folders can be called
    /// `Tokens`, and every caller (toggling it, indenting it, prefilling a new
    /// request's name with it) needs to know which one this is.
    Folder { path: Vec<String>, expanded: bool },
    /// A request, as an index into the collection's flat `entries`.
    Entry(usize),
}

impl Row {
    /// How far in to draw the row: the number of folders above it.
    pub fn depth(&self, entries: &[HurlEntry]) -> usize {
        match self {
            Self::Folder { path, .. } => path.len() - 1,
            Self::Entry(i) => entries
                .get(*i)
                .map(|e| entry_path(&e.title).len() - 1)
                .unwrap_or(0),
        }
    }
}

/// The rows to show for a collection whose expanded folders are `expanded`:
/// every folder and every request that no collapsed folder is hiding, **in the
/// order the file lists them**, a folder appearing where its first request
/// does.
///
/// The file is the source of truth for order: a `.hurl` collection is a
/// sequence, requests that share state have to run in sequence, and the author
/// put them in that sequence deliberately. Sorting the folders, or hoisting
/// them above the loose requests, would show a different collection to the one
/// on disk and to the one Run All executes.
///
/// A folder row is emitted the first time a request needs it, and only while
/// every folder above it is open -- so collapsing a folder hides its
/// subfolders as well as its requests, which is the whole point of collapsing
/// it.
pub fn rows_for(entries: &[HurlEntry], expanded: &HashSet<Vec<String>>) -> Vec<Row> {
    let items: Vec<(usize, Vec<String>)> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let path = entry_path(&e.title);
            (i, path[..path.len() - 1].to_vec())
        })
        .collect();
    let mut rows = Vec::with_capacity(entries.len() + 1);
    push_level(&items, &[], expanded, &mut rows);
    rows
}

/// Emit the rows for one level of the tree: `items` is every request at or
/// below `prefix`, paired with the folder path it lives in.
///
/// Each folder's requests are gathered under its row even when the file
/// interleaves them with other folders' — `A/one, B/one, A/two` puts both `A`
/// requests under `A`. Emitting them where the file lists them instead would
/// draw `A/two` below the `B` row, i.e. inside a folder it is not in. The
/// file's order still decides where each folder and each loose request lands,
/// and the order requests run in is the file's regardless of how they are
/// grouped on screen.
fn push_level(
    items: &[(usize, Vec<String>)],
    prefix: &[String],
    expanded: &HashSet<Vec<String>>,
    rows: &mut Vec<Row>,
) {
    let depth = prefix.len();
    let mut done: Vec<&String> = Vec::new();
    for (i, folders) in items {
        // Directly in this folder: a row of its own, in file order.
        if folders.len() == depth {
            rows.push(Row::Entry(*i));
            continue;
        }
        let name = &folders[depth];
        if done.contains(&name) {
            continue;
        }
        done.push(name);
        let mut path = prefix.to_vec();
        path.push(name.clone());
        let open = expanded.contains(&path);
        rows.push(Row::Folder {
            path: path.clone(),
            expanded: open,
        });
        if open {
            let inside: Vec<(usize, Vec<String>)> = items
                .iter()
                .filter(|(_, f)| f.len() > depth && &f[depth] == name)
                .cloned()
                .collect();
            push_level(&inside, &path, expanded, rows);
        }
    }
}

/// Every folder on the way down to `folder`, itself included: the set that has
/// to be open for a row inside it to be visible.
pub fn ancestors_of(folder: &[String]) -> HashSet<Vec<String>> {
    (0..folder.len()).map(|d| folder[..=d].to_vec()).collect()
}

/// The rows to show when the Requests list is being filtered by a typed query:
/// every request whose title contains `query`, case-insensitively, in original
/// order.
///
/// Deliberately **flat and folder-blind**, unlike [`rows_for`]. The list shows
/// one folder level at a time, so a filter that only looked inside the current
/// folder would fail at the one job it has — "find me that request" almost
/// always means finding one you can't currently see. There is no `Up` row and
/// there are no `Folder` rows: with the tree flattened there is nothing to
/// descend into, and a folder row that couldn't be entered would be a dead end.
/// Callers should show each match's *full* title, since two folders can hold
/// requests with the same leaf name and the folder rows that used to tell them
/// apart are gone.
///
/// Matching is against the whole title, folder segments included, so `auth/`
/// narrows to a folder just as readily as a request name does.
pub fn rows_matching(entries: &[HurlEntry], query: &str) -> Vec<Row> {
    let needle = query.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.title.to_lowercase().contains(&needle))
        .map(|(i, _)| Row::Entry(i))
        .collect()
}

/// The folder containing `entries[idx]` (all but the leaf segment of its
/// title path), or the root if `idx` is out of range.
pub fn folder_of(entries: &[HurlEntry], idx: usize) -> Vec<String> {
    let Some(e) = entries.get(idx) else {
        return Vec::new();
    };
    let mut path = entry_path(&e.title);
    path.pop();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set of open folders named as slash paths, for readability.
    fn open(paths: &[&str]) -> HashSet<Vec<String>> {
        paths
            .iter()
            .map(|p| p.split('/').map(str::to_string).collect())
            .collect()
    }

    fn folder(path: &str, expanded: bool) -> Row {
        Row::Folder {
            path: path.split('/').map(str::to_string).collect(),
            expanded,
        }
    }

    fn entry(title: &str) -> HurlEntry {
        HurlEntry {
            title: title.to_string(),
            method: "GET".to_string(),
            url: "http://x".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn entry_path_splits_on_slash_and_trims_segments() {
        assert_eq!(entry_path("Auth / Login"), vec!["Auth", "Login"]);
        assert_eq!(
            entry_path("Auth/Tokens/Refresh"),
            vec!["Auth", "Tokens", "Refresh"]
        );
        assert_eq!(entry_path("plain"), vec!["plain"]);
    }

    #[test]
    fn entry_path_of_an_untitled_or_slash_only_title_is_one_empty_segment() {
        assert_eq!(entry_path(""), vec![""]);
        assert_eq!(entry_path("///"), vec![""]);
    }

    #[test]
    fn root_rows_interleave_folders_and_requests_in_file_order() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Logout"),
            entry("Files/Upload/Big"),
        ];
        let rows = rows_for(&entries, &open(&[]));
        // `plain` is first because the file puts it first, and Auth precedes
        // Files for the same reason, not alphabetically. Closed folders show
        // as one row each and hide everything inside them.
        assert_eq!(
            rows,
            vec![Row::Entry(0), folder("Auth", false), folder("Files", false),]
        );
    }

    #[test]
    fn a_folder_keeps_the_position_of_its_first_request() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        assert_eq!(
            rows_for(&entries, &open(&[])),
            vec![folder("Zed", false), Row::Entry(1), folder("Abe", false)]
        );
    }

    /// The point of the tree: more than one folder can be open at a time, and
    /// each one's contents appear under it rather than replacing the list.
    #[test]
    fn several_folders_can_be_open_at_once() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Tokens/Refresh"),
            entry("Files/Upload/Big"),
        ];
        let rows = rows_for(&entries, &open(&["Auth", "Files"]));
        assert_eq!(
            rows,
            vec![
                Row::Entry(0),
                folder("Auth", true),
                Row::Entry(1),
                folder("Auth/Tokens", false),
                folder("Files", true),
                folder("Files/Upload", false),
            ],
            "opening Auth should not have closed Files"
        );
    }

    /// A folder's requests are drawn under it even when the file interleaves
    /// them with another folder's. Emitting each request where the file lists
    /// it put `Auth/Logout` below the `Users` row -- under a folder it is not
    /// in.
    #[test]
    fn a_folders_requests_are_gathered_under_it_even_when_the_file_interleaves_them() {
        let entries = vec![
            entry("Auth/Login"),
            entry("Users/List"),
            entry("Auth/Logout"),
        ];
        assert_eq!(
            rows_for(&entries, &open(&["Auth"])),
            vec![
                folder("Auth", true),
                Row::Entry(0),
                Row::Entry(2),
                folder("Users", false),
            ]
        );
    }

    /// A folder inside a closed folder is not drawn at all -- neither its own
    /// row nor its contents. Hiding the requests but leaving the subfolder
    /// rows behind would make a collapsed folder look half-open.
    #[test]
    fn a_closed_folder_hides_its_subfolders_as_well_as_its_requests() {
        let entries = vec![entry("Files/Upload/Big"), entry("Files/Upload/Small")];
        assert_eq!(rows_for(&entries, &open(&[])), vec![folder("Files", false)]);
        assert_eq!(
            rows_for(&entries, &open(&["Files"])),
            vec![folder("Files", true), folder("Files/Upload", false)]
        );
        assert_eq!(
            rows_for(&entries, &open(&["Files", "Files/Upload"])),
            vec![
                folder("Files", true),
                folder("Files/Upload", true),
                Row::Entry(0),
                Row::Entry(1),
            ]
        );
    }

    /// Opening a deep folder without its parent shows nothing new: the parent
    /// is what is hiding it, and an open child inside a closed parent is a
    /// state the user cannot see and so cannot undo.
    #[test]
    fn opening_a_folder_inside_a_closed_one_changes_nothing() {
        let entries = vec![entry("Files/Upload/Big")];
        assert_eq!(
            rows_for(&entries, &open(&["Files/Upload"])),
            vec![folder("Files", false)]
        );
    }

    /// How far in each row is drawn -- the folder rows above it.
    #[test]
    fn depth_counts_the_folders_above_a_row() {
        let entries = vec![entry("Files/Upload/Big"), entry("plain")];
        assert_eq!(folder("Files", false).depth(&entries), 0);
        assert_eq!(folder("Files/Upload", false).depth(&entries), 1);
        assert_eq!(Row::Entry(0).depth(&entries), 2);
        assert_eq!(Row::Entry(1).depth(&entries), 0);
    }

    #[test]
    fn the_ancestors_of_a_folder_are_every_step_down_to_it() {
        let path = vec!["Files".to_string(), "Upload".to_string()];
        assert_eq!(ancestors_of(&path), open(&["Files", "Files/Upload"]));
        assert!(ancestors_of(&[]).is_empty());
    }

    #[test]
    fn a_filtered_list_is_flat_and_matches_on_the_whole_title() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Logout"),
            entry("Files/Upload/Big"),
        ];
        // No Up row and no Folder rows: the tree is flattened, so there is
        // nothing left to descend into.
        assert_eq!(
            rows_matching(&entries, "log"),
            vec![Row::Entry(1), Row::Entry(2)]
        );
        // The folder segments are part of the haystack, so a folder name
        // narrows to that folder without needing a separate gesture.
        assert_eq!(
            rows_matching(&entries, "auth/"),
            vec![Row::Entry(1), Row::Entry(2)]
        );
        // Case-insensitive, like every other filter in the app.
        assert_eq!(rows_matching(&entries, "BIG"), vec![Row::Entry(3)]);
        assert!(rows_matching(&entries, "nothing").is_empty());
    }

    #[test]
    fn a_filter_reaches_requests_a_closed_folder_would_hide() {
        let entries = vec![entry("Auth/Login"), entry("Files/Upload/Big")];
        // With Files closed, `rows_for` does not show its request at all —
        // which is exactly the case a filter exists to solve.
        let browsing = rows_for(&entries, &open(&["Auth"]));
        assert_eq!(
            browsing,
            vec![folder("Auth", true), Row::Entry(0), folder("Files", false)]
        );
        assert_eq!(rows_matching(&entries, "upload"), vec![Row::Entry(1)]);
    }

    #[test]
    fn an_all_whitespace_query_matches_everything_rather_than_nothing() {
        let entries = vec![entry("Auth/Login"), entry("plain")];
        // The query is trimmed, so a lone space (mid-typing, or left behind by
        // a backspace) must not read as "no request contains a space".
        assert_eq!(
            rows_matching(&entries, "   "),
            vec![Row::Entry(0), Row::Entry(1)]
        );
    }

    /// Sorting is only ever applied to the flat, filtered list: a tree sorted
    /// as one sequence would tear requests away from the folder they are drawn
    /// under.
    #[test]
    fn sorting_orders_a_flat_list_of_matches() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        let mut rows = rows_matching(&entries, "");
        sort_rows(&mut rows, &entries, SortMode::Alpha);
        assert_eq!(rows, vec![Row::Entry(1), Row::Entry(0), Row::Entry(2)]);

        sort_rows(&mut rows, &entries, SortMode::ReverseAlpha);
        assert_eq!(rows, vec![Row::Entry(2), Row::Entry(0), Row::Entry(1)]);
    }

    #[test]
    fn file_order_is_restored_by_switching_back_rather_than_left_half_sorted() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        let mut rows = rows_matching(&entries, "");
        let original = rows.clone();
        sort_rows(&mut rows, &entries, SortMode::Alpha);
        // `File` compares every pair equal and the sort is stable, so it is a
        // no-op over whatever order the rows are already in — which only gets
        // back to the file's order because `Collection::rows` rebuilds them.
        sort_rows(&mut rows, &entries, SortMode::File);
        assert_ne!(rows, original);
        let mut fresh = rows_matching(&entries, "");
        sort_rows(&mut fresh, &entries, SortMode::File);
        assert_eq!(fresh, original);
    }

    #[test]
    fn the_sort_button_cycles_back_to_file_order() {
        assert_eq!(SortMode::default(), SortMode::File);
        assert_eq!(SortMode::File.next(), SortMode::Alpha);
        assert_eq!(SortMode::Alpha.next(), SortMode::ReverseAlpha);
        assert_eq!(SortMode::ReverseAlpha.next(), SortMode::File);
    }

    #[test]
    fn folder_of_returns_the_parent_path_of_an_entry() {
        let entries = vec![entry("plain"), entry("Auth/Tokens/Refresh")];
        assert_eq!(folder_of(&entries, 0), Vec::<String>::new());
        assert_eq!(
            folder_of(&entries, 1),
            vec!["Auth".to_string(), "Tokens".to_string()]
        );
        assert_eq!(folder_of(&entries, 99), Vec::<String>::new());
    }
}
