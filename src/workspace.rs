//! Recursive folder scanning for the Workspaces feature: loading a folder
//! containing many `.hurl`/Postman-JSON collection files into a single tab,
//! and letting the user browse/choose which one to view (see the
//! `Overlay::WorkspacePicker` popup in `crate::tui`).
//!
//! This is deliberately independent from [`crate::tree`], which is about
//! folder paths *encoded in a request's title* inside one already-loaded
//! collection — this module instead walks the real filesystem under a
//! chosen root directory.

use std::path::{Path, PathBuf};

/// Filesystem entries recurse at most this many levels deep below the
/// workspace root, as a defensive guard against pathological symlink loops.
const MAX_DEPTH: usize = 32;

/// One row of a flattened, depth-first workspace file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsEntry {
    /// Full filesystem path.
    pub path: PathBuf,
    /// Just the file/dir name (not the full path), for display.
    pub display_name: String,
    /// Nesting depth below the root (root's direct children are depth 0).
    pub depth: usize,
    pub is_dir: bool,
}

/// Recursively scans `root`, returning a flattened, depth-first list of
/// entries (each directory immediately followed by its children) — folders
/// sorted before files at each level, then alphabetically within each group.
///
/// When `filter_hurl_json` is `true`, only `.hurl`/`.json` files are
/// included, and any directory whose subtree contains none of those files is
/// omitted entirely (so an unrelated folder full of other file types doesn't
/// clutter the tree). When `false`, every non-hidden file is shown and no
/// directory is hidden for being "empty" of matches.
///
/// Hidden entries (dot-prefixed names, e.g. `.git`) are always excluded.
/// Unreadable directories are silently skipped rather than failing the
/// whole scan.
pub fn scan_workspace(root: &Path, filter_hurl_json: bool) -> Vec<WsEntry> {
    let mut out = Vec::new();
    scan_dir(root, 0, filter_hurl_json, &mut out);
    out
}

fn scan_dir(dir: &Path, depth: usize, filter_hurl_json: bool, out: &mut Vec<WsEntry>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            dirs.push(path);
        } else if is_matching_file(&path, filter_hurl_json) {
            files.push(path);
        }
    }
    dirs.sort();
    files.sort();

    for d in dirs {
        // Recurse first into a scratch buffer so we can decide whether this
        // directory is worth showing at all before committing any rows.
        let mut sub = Vec::new();
        scan_dir(&d, depth + 1, filter_hurl_json, &mut sub);
        if filter_hurl_json && sub.is_empty() {
            continue;
        }
        let display_name = d
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        out.push(WsEntry {
            path: d,
            display_name,
            depth,
            is_dir: true,
        });
        out.extend(sub);
    }
    for f in files {
        let display_name = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        out.push(WsEntry {
            path: f,
            display_name,
            depth,
            is_dir: false,
        });
    }
}

/// Non-recursive scan of a single directory's immediate children: its
/// subfolders (alphabetical) followed by its `.hurl`/`.json` collection files
/// (alphabetical), used by the Workspace tab's file-tree request list to
/// browse one folder at a time. Unlike [`scan_workspace`] this does not
/// recurse into the returned subfolders (their `depth` is always 0), but when
/// `filter_hurl_json` is true a subfolder is still only listed if its subtree
/// contains at least one collection file — so empty or unrelated folders
/// don't clutter the browse view. Hidden dot-prefixed entries are always
/// skipped; an unreadable directory yields an empty list.
pub fn list_dir(dir: &Path, filter_hurl_json: bool) -> Vec<WsEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if filter_hurl_json {
                let mut sub = Vec::new();
                scan_dir(&path, 1, true, &mut sub);
                if sub.is_empty() {
                    continue;
                }
            }
            dirs.push(path);
        } else if is_matching_file(&path, filter_hurl_json) {
            files.push(path);
        }
    }
    dirs.sort();
    files.sort();
    let to_entry = |path: PathBuf, is_dir: bool| WsEntry {
        display_name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string(),
        path,
        depth: 0,
        is_dir,
    };
    let mut out = Vec::with_capacity(dirs.len() + files.len());
    out.extend(dirs.into_iter().map(|d| to_entry(d, true)));
    out.extend(files.into_iter().map(|f| to_entry(f, false)));
    out
}

/// Whether `path`'s extension matches the collection-file filter (case
/// insensitive `.hurl`/`.json`) — always `true` when the filter is off.
fn is_matching_file(path: &Path, filter_hurl_json: bool) -> bool {
    if !filter_hurl_json {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("hurl") || ext.eq_ignore_ascii_case("json"),
        None => false,
    }
}

/// Recursively copies `src`'s contents into `dst` (creating `dst` and any
/// needed subdirectories), skipping hidden (dot-prefixed) entries exactly
/// like [`scan_workspace`] — used by "Save Workspace" to copy a Workspace's
/// files to a new permanent location without dragging along an internal
/// `.git` folder (from a git download) or other clutter. Copies the whole
/// visible tree regardless of the workspace's own `.hurl`/`.json` display
/// filter, since that filter only controls what the picker *shows*, not
/// what belongs to the workspace.
pub fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir_all_inner(src, dst, 0)
}

fn copy_dir_all_inner(src: &Path, dst: &Path, depth: usize) -> std::io::Result<()> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let dest_path = dst.join(name);
        if path.is_dir() {
            copy_dir_all_inner(&path, &dest_path, depth + 1)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

/// Walk `root`'s visible tree and collect every non-hidden file as a
/// `(repo-relative path, UTF-8 contents)` pair, ready to hand to
/// [`crate::git_remote::commit_files`] for "Save Workspace to Git". Paths use
/// `/` separators (git's on-wire form) regardless of platform. Hidden
/// (dot-prefixed) entries — notably a git download's internal `.git` folder —
/// are skipped exactly like [`copy_dir_all`], and any file that isn't valid
/// UTF-8 is skipped defensively (the git commit plumbing only writes text
/// blobs; a workspace is expected to hold `.hurl`/`.json`/`.vars` text). The
/// result is sorted by path for a deterministic commit.
pub fn collect_files_for_commit(root: &Path) -> std::io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    collect_files_inner(root, root, 0, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect_files_inner(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_files_inner(root, &path, depth + 1, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            // Skip anything that isn't valid UTF-8 text rather than failing
            // the whole commit.
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let repo_path = rel
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((repo_path, contents));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperboy_workspace_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_files_and_subfolders_recursively_dirs_first_then_files_alphabetically() {
        let root = tmp_dir("basic");
        fs::write(root.join("b.hurl"), "").unwrap();
        fs::write(root.join("a.hurl"), "").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/c.hurl"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, vec!["sub", "c.hurl", "a.hurl", "b.hurl"]);
        assert_eq!(entries[0].depth, 0);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].depth, 1);
        assert!(!entries[1].is_dir);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_files_for_commit_gathers_the_whole_visible_tree_with_slash_paths() {
        let root = tmp_dir("collect_commit");
        fs::write(root.join("a.hurl"), "GET a\n").unwrap();
        fs::create_dir_all(root.join("api")).unwrap();
        fs::write(root.join("api/b.hurl"), "GET b\n").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "secret\n").unwrap();

        let mut files = collect_files_for_commit(&root).unwrap();
        files.sort();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a.hurl", "api/b.hurl"]);
        assert_eq!(files[1].1, "GET b\n");
        assert!(
            !paths.iter().any(|p| p.contains(".git")),
            "dot-prefixed entries are never collected"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_files_for_commit_skips_non_utf8_files_rather_than_failing() {
        let root = tmp_dir("collect_binary");
        fs::write(root.join("ok.hurl"), "GET ok\n").unwrap();
        fs::write(root.join("blob.bin"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

        let files = collect_files_for_commit(&root).unwrap();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["ok.hurl"], "the binary file is skipped");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_on_only_includes_hurl_and_json_files_case_insensitively() {
        let root = tmp_dir("filter_on");
        fs::write(root.join("keep.hurl"), "").unwrap();
        fs::write(root.join("KEEP.JSON"), "").unwrap();
        fs::write(root.join("skip.txt"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert!(names.contains(&"keep.hurl"));
        assert!(names.contains(&"KEEP.JSON"));
        assert!(!names.contains(&"skip.txt"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_off_shows_every_non_hidden_file_and_never_hides_empty_directories() {
        let root = tmp_dir("filter_off");
        fs::write(root.join("notes.txt"), "").unwrap();
        fs::create_dir_all(root.join("empty_sub")).unwrap();

        let entries = scan_workspace(&root, false);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert!(names.contains(&"notes.txt"));
        assert!(names.contains(&"empty_sub"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_on_hides_directories_whose_subtree_has_no_matching_files() {
        let root = tmp_dir("hide_empty");
        fs::create_dir_all(root.join("irrelevant")).unwrap();
        fs::write(root.join("irrelevant/notes.txt"), "").unwrap();
        fs::create_dir_all(root.join("relevant")).unwrap();
        fs::write(root.join("relevant/req.hurl"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert!(
            !names.contains(&"irrelevant"),
            "a folder with no matching descendants is hidden when filtered"
        );
        assert!(names.contains(&"relevant"));
        assert!(names.contains(&"req.hurl"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_dot_prefixed_entries_are_always_excluded() {
        let root = tmp_dir("hidden");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "").unwrap();
        fs::write(root.join(".hidden.hurl"), "").unwrap();
        fs::write(root.join("visible.hurl"), "").unwrap();

        for filter in [true, false] {
            let entries = scan_workspace(&root, filter);
            let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
            assert!(!names.contains(&".git"));
            assert!(!names.contains(&".hidden.hurl"));
            assert!(names.contains(&"visible.hurl"));
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deeply_nested_folders_are_flattened_depth_first_with_correct_depth_numbers() {
        let root = tmp_dir("nested");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/b/c/deep.hurl"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let depths: Vec<(String, usize)> = entries
            .iter()
            .map(|e| (e.display_name.clone(), e.depth))
            .collect();
        assert_eq!(
            depths,
            vec![
                ("a".to_string(), 0),
                ("b".to_string(), 1),
                ("c".to_string(), 2),
                ("deep.hurl".to_string(), 3)
            ]
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scanning_a_missing_root_yields_an_empty_list_rather_than_panicking() {
        let root = std::env::temp_dir().join("paperboy_workspace_test_definitely_missing_xyz");
        let _ = fs::remove_dir_all(&root);
        assert_eq!(scan_workspace(&root, true), Vec::new());
    }

    #[test]
    fn copy_dir_all_copies_nested_files_and_folders_but_skips_hidden_entries() {
        let src = tmp_dir("copy_src");
        let dst = std::env::temp_dir().join(format!(
            "paperboy_workspace_test_copy_dst_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dst);

        fs::write(src.join("a.hurl"), "GET https://example.com/a\n").unwrap();
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/b.json"), "{}").unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(src.join(".hidden"), "nope").unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("a.hurl")).unwrap(),
            "GET https://example.com/a\n"
        );
        assert_eq!(fs::read_to_string(dst.join("sub/b.json")).unwrap(), "{}");
        assert!(
            !dst.join(".git").exists(),
            "hidden dot-prefixed folders (like .git) are never copied"
        );
        assert!(!dst.join(".hidden").exists());

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    #[test]
    fn copy_dir_all_creates_the_destination_even_for_an_empty_source() {
        let src = tmp_dir("copy_empty_src");
        let dst = std::env::temp_dir().join(format!(
            "paperboy_workspace_test_copy_empty_dst_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dst);

        copy_dir_all(&src, &dst).unwrap();
        assert!(dst.is_dir());

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }
}
