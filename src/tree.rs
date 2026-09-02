//! Folder-structure support for collections. Neither Hurl nor our own
//! `HurlEntry` model has a real notion of folders, but a request's `title`
//! can encode one by using `/` as a path separator (e.g. `"Auth/Login"`,
//! `"Auth/Tokens/Refresh"`) — the same convention Postman collections use for
//! nested folders once imported (see [`crate::postman::import_postman`]).
//! This module turns that convention into a flat, navigable view: at any
//! given folder, you see its direct subfolders and direct requests, never a
//! deep indented tree.

use std::cmp::Ordering;

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

/// Reorder `rows` in place under `mode`.
///
/// `Row::Up` is pinned to the top whichever way the rest is ordered: it is the
/// way out of the folder, not one of its contents, and a "go up" row that sank
/// to the bottom under Z-A would be a trap. Folders and requests sort together
/// by the name on screen rather than in separate blocks, so A-Z means what it
/// looks like.
pub fn sort_rows(rows: &mut [Row], entries: &[HurlEntry], mode: SortMode) {
    let name = |row: &Row| match row {
        Row::Up => String::new(),
        Row::Folder(n) => n.clone(),
        Row::Entry(i) => entries.get(*i).map(leaf_name).unwrap_or_default(),
    };
    rows.sort_by(|a, b| {
        let pinned = |r: &Row| u8::from(!matches!(r, Row::Up));
        pinned(a)
            .cmp(&pinned(b))
            .then_with(|| cmp_names(mode, &name(a), &name(b)))
    });
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
    /// Go up to the parent folder (only present when not at the root).
    Up,
    /// Descend into a subfolder of the current one.
    Folder(String),
    /// A request directly in the current folder (index into the collection's
    /// flat `entries`).
    Entry(usize),
}

/// The rows to show for `folder` (the current breadcrumb path, root = `[]`):
/// an optional `Up` row, then this folder's direct requests and subfolders
/// **in the order the file lists them**, a subfolder appearing where its first
/// request does.
///
/// The file is the source of truth for order: a `.hurl` collection is a
/// sequence, requests that share state have to run in sequence, and the author
/// put them in that sequence deliberately. Sorting the folders, or hoisting
/// them above the loose requests, would show a different collection to the one
/// on disk and to the one Run All executes.
pub fn rows_for(entries: &[HurlEntry], folder: &[String]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(entries.len() + 1);
    if !folder.is_empty() {
        rows.push(Row::Up);
    }
    for (i, e) in entries.iter().enumerate() {
        let path = entry_path(&e.title);
        if path.len() <= folder.len() || path[..folder.len()] != *folder {
            continue;
        }
        if path.len() == folder.len() + 1 {
            rows.push(Row::Entry(i));
        } else {
            let row = Row::Folder(path[folder.len()].clone());
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    rows
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
        let rows = rows_for(&entries, &[]);
        // `plain` is first because the file puts it first, and Auth precedes
        // Files for the same reason, not alphabetically.
        assert_eq!(
            rows,
            vec![
                Row::Entry(0),
                Row::Folder("Auth".into()),
                Row::Folder("Files".into()),
            ]
        );
    }

    #[test]
    fn a_folder_keeps_the_position_of_its_first_request() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        assert_eq!(
            rows_for(&entries, &[]),
            vec![
                Row::Folder("Zed".into()),
                Row::Entry(1),
                Row::Folder("Abe".into()),
            ]
        );
    }

    #[test]
    fn descending_a_folder_shows_its_direct_children_with_an_up_row() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Tokens/Refresh"),
            entry("Auth/Logout"),
        ];
        let rows = rows_for(&entries, &["Auth".to_string()]);
        assert_eq!(
            rows,
            vec![
                Row::Up,
                Row::Entry(1),
                Row::Folder("Tokens".into()),
                Row::Entry(3)
            ]
        );
    }

    #[test]
    fn nested_folders_stay_flat_at_each_level() {
        let entries = vec![entry("Files/Upload/Big"), entry("Files/Upload/Small")];
        let rows = rows_for(&entries, &["Files".to_string(), "Upload".to_string()]);
        assert_eq!(rows, vec![Row::Up, Row::Entry(0), Row::Entry(1)]);
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
    fn a_filter_reaches_requests_the_current_folder_would_hide() {
        let entries = vec![entry("Auth/Login"), entry("Files/Upload/Big")];
        // Browsing inside Auth, `rows_for` cannot see the Files request at all
        // — which is exactly the case a filter exists to solve.
        let browsing = rows_for(&entries, &["Auth".to_string()]);
        assert_eq!(browsing, vec![Row::Up, Row::Entry(0)]);
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

    #[test]
    fn sorting_orders_folders_and_requests_together_and_pins_the_up_row() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        let mut rows = rows_for(&entries, &[]);

        sort_rows(&mut rows, &entries, SortMode::Alpha);
        assert_eq!(
            rows,
            vec![
                Row::Folder("Abe".into()),
                Row::Entry(1),
                Row::Folder("Zed".into()),
            ],
            "folders and requests sort into one A-Z run, not separate blocks"
        );

        sort_rows(&mut rows, &entries, SortMode::ReverseAlpha);
        assert_eq!(
            rows,
            vec![
                Row::Folder("Zed".into()),
                Row::Entry(1),
                Row::Folder("Abe".into()),
            ]
        );

        // The Up row is the way out of a folder, not one of its contents, so
        // it stays on top even when everything else is reversed.
        let mut nested = rows_for(&entries, &["Zed".to_string()]);
        sort_rows(&mut nested, &entries, SortMode::ReverseAlpha);
        assert_eq!(nested.first(), Some(&Row::Up));
    }

    #[test]
    fn file_order_is_restored_by_switching_back_rather_than_left_half_sorted() {
        let entries = vec![entry("Zed/One"), entry("loose"), entry("Abe/Two")];
        let mut rows = rows_for(&entries, &[]);
        let original = rows.clone();
        sort_rows(&mut rows, &entries, SortMode::Alpha);
        // `File` compares every pair equal and the sort is stable, so it is a
        // no-op over whatever order the rows are already in — which only gets
        // back to the file's order because `Collection::rows` rebuilds them.
        sort_rows(&mut rows, &entries, SortMode::File);
        assert_ne!(rows, original);
        let mut fresh = rows_for(&entries, &[]);
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
