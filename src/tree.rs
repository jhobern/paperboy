//! Folder-structure support for collections. Neither Hurl nor our own
//! `HurlEntry` model has a real notion of folders, but a request's `title`
//! can encode one by using `/` as a path separator (e.g. `"Auth/Login"`,
//! `"Auth/Tokens/Refresh"`) — the same convention Postman collections use for
//! nested folders once imported (see [`crate::postman::import_postman`]).
//! This module turns that convention into a flat, navigable view: at any
//! given folder, you see its direct subfolders and direct requests, never a
//! deep indented tree.

use crate::hurl::HurlEntry;

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
/// an optional `Up` row, then every direct subfolder (alphabetical,
/// deduplicated), then every direct request, in original order.
pub fn rows_for(entries: &[HurlEntry], folder: &[String]) -> Vec<Row> {
    let mut folders: Vec<String> = Vec::new();
    let mut leaves: Vec<usize> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let path = entry_path(&e.title);
        if path.len() <= folder.len() || path[..folder.len()] != *folder {
            continue;
        }
        if path.len() == folder.len() + 1 {
            leaves.push(i);
        } else {
            let name = &path[folder.len()];
            if !folders.contains(name) {
                folders.push(name.clone());
            }
        }
    }
    folders.sort();
    let mut rows = Vec::with_capacity(folders.len() + leaves.len() + 1);
    if !folder.is_empty() {
        rows.push(Row::Up);
    }
    rows.extend(folders.into_iter().map(Row::Folder));
    rows.extend(leaves.into_iter().map(Row::Entry));
    rows
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
    fn root_rows_show_top_level_requests_and_folders_without_an_up_row() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Logout"),
            entry("Files/Upload/Big"),
        ];
        let rows = rows_for(&entries, &[]);
        assert_eq!(
            rows,
            vec![
                Row::Folder("Auth".into()),
                Row::Folder("Files".into()),
                Row::Entry(0)
            ]
        );
    }

    #[test]
    fn descending_a_folder_shows_its_direct_children_with_an_up_row() {
        let entries = vec![
            entry("plain"),
            entry("Auth/Login"),
            entry("Auth/Logout"),
            entry("Auth/Tokens/Refresh"),
        ];
        let rows = rows_for(&entries, &["Auth".to_string()]);
        assert_eq!(
            rows,
            vec![
                Row::Up,
                Row::Folder("Tokens".into()),
                Row::Entry(1),
                Row::Entry(2)
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
