//! Recursive folder scanning for the Workspaces feature: loading a folder
//! containing many `.hurl`/Postman-JSON collection files into a single tab,
//! and letting the user browse/choose which one to view (see the
//! `Overlay::WorkspacePicker` popup in `crate::tui`).
//!
//! This is deliberately independent from [`crate::tree`], which is about
//! folder paths *encoded in a request's title* inside one already-loaded
//! collection — this module instead walks the real filesystem under a
//! chosen root directory.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Filesystem entries recurse at most this many levels deep below the
/// workspace root, as a defensive guard against pathological symlink loops.
const MAX_DEPTH: usize = 32;

/// Bumped every time PaperBoy itself changes the shape of a workspace tree.
///
/// [`Collection::ws_rows`](crate::collection::Collection::ws_rows) reuses a
/// scan for a short while rather than reading the disk on every frame, but
/// PaperBoy's *own* edits have to show up at once — creating a file and then
/// not finding it in the tree is a bug, not a stale cache. So the write helpers
/// here announce themselves, and a cache taken at an older generation is
/// discarded on the spot.
///
/// It is a plain counter rather than a set of invalidated paths because the
/// question a cache asks is only ever "is what I have still the latest?", and
/// because a missed bump is merely slow (the scan's own expiry catches it)
/// rather than wrong.
static TREE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The current tree generation; store it beside a cached scan and re-read when
/// it no longer matches.
pub fn tree_generation() -> u64 {
    TREE_GENERATION.load(Ordering::Relaxed)
}

/// Announce that the workspace tree has changed, so any cached scan is dropped
/// at the next redraw. Called by the write helpers below; call it directly
/// after writing a workspace file by some other route.
pub fn note_tree_changed() {
    TREE_GENERATION.fetch_add(1, Ordering::Relaxed);
}

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

/// The kinds of thing a workspace is made of, as something the user can ask for
/// a *new* one of: the three file types, plus the folder they get organised
/// into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NewItemKind {
    Collection,
    Report,
    Environment,
    /// A subfolder. Unlike the file kinds it has no extension and no starter
    /// content -- it exists purely to be dragged things into.
    Folder,
}

impl NewItemKind {
    /// The extension given to a name typed without one. Empty for a folder,
    /// which never gains one.
    pub fn extension(self) -> &'static str {
        match self {
            NewItemKind::Collection => "hurl",
            NewItemKind::Report => "trail",
            NewItemKind::Environment => "vars",
            NewItemKind::Folder => "",
        }
    }

    /// Which kind of file a name describes, by its extension.
    ///
    /// The three kinds are already told apart by extension everywhere else in
    /// the app (that is how the tree decides what each row is), so a keyboard
    /// front-end can ask for one name rather than first asking which of three
    /// things is being made. `None` for anything that isn't a workspace file
    /// type; a name with no extension at all is a collection, which is both the
    /// commonest case and the existing default.
    pub fn from_name(name: &str) -> Option<Self> {
        match Path::new(name).extension().and_then(|e| e.to_str()) {
            None => Some(NewItemKind::Collection),
            Some(e) => match e.to_ascii_lowercase().as_str() {
                "hurl" | "json" => Some(NewItemKind::Collection),
                "trail" => Some(NewItemKind::Report),
                "vars" => Some(NewItemKind::Environment),
                _ => None,
            },
        }
    }

    /// What a brand-new file of this kind contains.
    ///
    /// Never empty: an empty file is indistinguishable from a broken one, and
    /// every one of these formats treats `#` as a comment, so a one-line note
    /// naming the file is both valid and a hint about what goes in it.
    fn starter(self, stem: &str) -> String {
        match self {
            NewItemKind::Collection => format!("# {stem}\n"),
            // A report has real structure, so it gets the same template a
            // scratch report in a tab does rather than a bare comment.
            NewItemKind::Report => crate::report::Report::scratch(stem).text,
            NewItemKind::Environment => format!("# {stem}\n"),
            // Unused: `create_item` makes a directory rather than writing a
            // file for this kind.
            NewItemKind::Folder => String::new(),
        }
    }
}

/// Create a new, empty collection / report / environment inside a workspace.
///
/// `dir` is the folder it should land in (the workspace root itself, or a
/// folder the user right-clicked); `name` is what they typed, which may include
/// subfolders and may omit the extension.
///
/// Refuses anything that would leave `root` — both lexically (an absolute path
/// or a `..` segment) and physically (a symlinked component that *resolves*
/// outside, which the lexical check can't see). A workspace is a self-contained
/// thing that gets copied and shared as a unit, so a file that appears to be in
/// the tree but actually lives elsewhere on disk would silently not travel with
/// it. Refuses to overwrite an existing file for the same reason it's easy to
/// do by accident: the names in a workspace are short and repetitive.
pub fn create_item(
    root: &Path,
    dir: &Path,
    name: &str,
    kind: NewItemKind,
) -> Result<PathBuf, NewItemError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NewItemError::EmptyName);
    }
    let mut rel = PathBuf::from(name);
    // A folder keeps exactly the name that was typed -- "v2" must not become
    // "v2.hurl", and a dot in a folder name is the user's business.
    if kind != NewItemKind::Folder && rel.extension().is_none() {
        rel.set_extension(kind.extension());
    }
    let lexically_safe = rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !lexically_safe {
        return Err(NewItemError::Escapes(rel.display().to_string()));
    }

    let full = dir.join(&rel);
    if escapes_root(root, &full) {
        return Err(NewItemError::Escapes(full.display().to_string()));
    }
    if full.exists() {
        return Err(NewItemError::Exists(display_name(root, &full)));
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| NewItemError::Io(format!("{}: {e}", parent.display())))?;
    }
    if kind == NewItemKind::Folder {
        std::fs::create_dir_all(&full)
            .map_err(|e| NewItemError::Io(format!("{}: {e}", full.display())))?;
        note_tree_changed();
        return Ok(full);
    }
    let stem = full
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    std::fs::write(&full, kind.starter(&stem))
        .map_err(|e| NewItemError::Io(format!("{}: {e}", full.display())))?;
    note_tree_changed();
    Ok(full)
}

/// Move a workspace file or folder into `dest_dir`, keeping its name.
///
/// Refuses anything that would take the item out of `root` (in either
/// direction) for the same reason [`create_item`] does: a workspace travels as
/// a unit, and an item that appears in the tree but lives elsewhere on disk
/// would silently not travel with it. Also refuses to move a folder into itself
/// or its own descendant — `fs::rename` would either fail obscurely or, worse,
/// succeed and lose the subtree.
///
/// Returns the item's new path. Moving something to where it already is is not
/// an error; it just does nothing, which is what dropping a file back on its
/// own folder should do.
pub fn move_item(root: &Path, src: &Path, dest_dir: &Path) -> Result<PathBuf, MoveError> {
    if !src.starts_with(root) || escapes_root(root, src) {
        return Err(MoveError::Escapes(src.display().to_string()));
    }
    if !dest_dir.starts_with(root) || escapes_root(root, dest_dir) {
        return Err(MoveError::Escapes(dest_dir.display().to_string()));
    }
    let Some(name) = src.file_name() else {
        return Err(MoveError::Escapes(src.display().to_string()));
    };
    // Dropping an item on the folder it is already in.
    if src.parent() == Some(dest_dir) {
        return Ok(src.to_path_buf());
    }
    if dest_dir.starts_with(src) {
        return Err(MoveError::IntoItself);
    }
    let dest = dest_dir.join(name);
    if dest.exists() {
        return Err(MoveError::Exists(display_name(root, &dest)));
    }
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| MoveError::Io(format!("{}: {e}", dest_dir.display())))?;
    std::fs::rename(src, &dest).map_err(|e| MoveError::Io(format!("{}: {e}", dest.display())))?;
    note_tree_changed();
    Ok(dest)
}

/// Why an item couldn't be moved. Separate variants for the same reason as
/// [`NewItemError`]: "there's already one of those there" and "that would leave
/// the workspace" need different words.
#[derive(Clone, Debug, PartialEq)]
pub enum MoveError {
    Escapes(String),
    Exists(String),
    IntoItself,
    Io(String),
}

/// Rename the workspace file or folder at `src` to `new_name`, keeping it in
/// the same folder.
///
/// `new_name` is a *name*, not a path: a separator or a `..` in it is refused
/// outright, because "rename" must not become "move anywhere on disk" — moving
/// an item between folders is [`move_item`]'s job, and it alone carries the
/// checks that a move needs (into-itself, cross-folder collisions). This mirrors
/// the lexical guard [`create_item`] puts on the leaf it is about to write, and
/// is deliberately stricter: a rename target is a single path component, full
/// stop.
///
/// Renaming to the name it already has is a no-op success rather than a
/// self-collision error — the item is returned unchanged. Renaming onto a name
/// something else already holds fails instead of clobbering it, for the same
/// reason [`create_item`] won't overwrite: workspace names are short and
/// repetitive, so an accidental collision is easy and losing the other file to
/// it would be silent.
///
/// ## What happens to a file's extension
///
/// A file **keeps the extension it had**. The extension is not decoration on
/// the name — it is how the tree decides what the file *is*: `scan_workspace`
/// (via [`is_workspace_file`]) only surfaces `.hurl`/`.json`/`.vars`/`.trail`,
/// and [`is_report_file`]/[`is_env_file`] tell a report from an environment
/// from a collection by it. So a rename that dropped or changed the extension
/// would quietly change the file's *kind*, or — worse — make it a kind the
/// scanner doesn't recognise at all, at which point the file simply vanishes
/// from the tree the next time it is drawn. That looks exactly like the rename
/// deleted it. Renaming is renaming; it must not be able to do that. Concretely:
///
/// - a typed name with no extension regains the source's (`billing.hurl` →
///   "invoices" ⇒ `invoices.hurl`), exactly as [`create_item`] fills one in;
/// - a typed name already ending in the source's extension is taken as-is
///   (`report.trail` → "nightly.trail" ⇒ `nightly.trail`);
/// - a typed name ending in some *other* extension has the source's re-appended
///   rather than substituted (`report.trail` → "nightly.bak" ⇒
///   `nightly.bak.trail`). Appending rather than replacing so that neither the
///   text the user typed (a bare `set_extension` would turn "data.v2" into
///   "data.hurl", losing the "v2") nor the file's kind is lost.
///
/// A folder has no extension and never gains one, so its typed name is used
/// verbatim — the same rule [`create_item`] applies to a new folder.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn rename_item(root: &Path, src: &Path, new_name: &str) -> Result<PathBuf, RenameError> {
    if !src.starts_with(root) || escapes_root(root, src) {
        return Err(RenameError::Escapes(src.display().to_string()));
    }
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(RenameError::EmptyName);
    }
    // A rename target is exactly one path component: no separators, no `..`, no
    // absolute prefix. Anything else is an attempt to move the item elsewhere
    // under the guise of renaming it.
    let candidate = Path::new(new_name);
    let mut comps = candidate.components();
    let single_component =
        matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
    if !single_component {
        return Err(RenameError::Escapes(new_name.to_string()));
    }

    let is_dir = src.is_dir();
    let final_name = if is_dir {
        new_name.to_string()
    } else {
        preserve_extension(src, new_name)
    };

    let Some(parent) = src.parent() else {
        return Err(RenameError::Escapes(src.display().to_string()));
    };
    let dest = parent.join(&final_name);

    // Renaming to the name it already has: nothing to do, and reporting a
    // collision against itself would be nonsense.
    if dest == src {
        return Ok(src.to_path_buf());
    }
    if escapes_root(root, &dest) {
        return Err(RenameError::Escapes(dest.display().to_string()));
    }
    if dest.exists() {
        return Err(RenameError::Exists(display_name(root, &dest)));
    }
    std::fs::rename(src, &dest).map_err(|e| RenameError::Io(format!("{}: {e}", dest.display())))?;
    note_tree_changed();
    Ok(dest)
}

/// Fold the source file's extension back into a typed rename `name` (see
/// [`rename_item`] for why the file's kind must survive a rename). Returns the
/// name unchanged when the source has no extension to preserve.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn preserve_extension(src: &Path, name: &str) -> String {
    let Some(src_ext) = src.extension().and_then(|e| e.to_str()) else {
        return name.to_string();
    };
    let already = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(src_ext));
    if already {
        name.to_string()
    } else {
        format!("{name}.{src_ext}")
    }
}

/// Why an item couldn't be renamed. A dedicated enum, like [`MoveError`] and
/// [`NewItemError`]: "there's already one of those there" and "that name would
/// leave the workspace" are different things to tell someone, and each
/// front-end phrases them itself.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum RenameError {
    /// Nothing was typed (or the dialog was dismissed): not worth reporting.
    EmptyName,
    Escapes(String),
    Exists(String),
    Io(String),
}

/// Delete the workspace file or folder at `path`. A folder is removed with
/// everything inside it; a file is removed on its own.
///
/// Refuses to touch anything outside `root` — both lexically and once symlinks
/// are resolved, exactly as [`create_item`] and [`move_item`] do — because a
/// recursive delete pointed even slightly wrong is the most destructive thing
/// this module can do. It also refuses to delete `root` itself: "delete this
/// item" must never be able to become "delete the whole workspace" (an empty
/// or root-equal `path` slipping through would do precisely that).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn delete_item(root: &Path, path: &Path) -> Result<(), DeleteError> {
    if !path.starts_with(root) || escapes_root(root, path) {
        return Err(DeleteError::Escapes(path.display().to_string()));
    }
    // Compare canonicalised where possible so a differently-spelled but
    // equivalent path (a trailing slash, a `.` component, a symlink to the
    // root) can't sneak the root itself past the guard; fall back to a lexical
    // compare when either can't be canonicalised.
    let is_root = match (root.canonicalize(), path.canonicalize()) {
        (Ok(r), Ok(p)) => r == p,
        _ => path == root,
    };
    if is_root {
        return Err(DeleteError::IsRoot);
    }
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| DeleteError::Io(format!("{}: {e}", path.display())))?;
    note_tree_changed();
    Ok(())
}

/// How many files live under `path` (recursively) — or `1` for a file. Used by
/// the delete confirmation to say *how much* a folder delete is about to
/// destroy, since a folder can take many files with it and a count is the
/// difference between an informed "yes" and a leap. Counts every file, not just
/// the workspace's own kinds, because the recursive delete removes all of them;
/// hidden entries and depth are bounded exactly as [`scan_workspace`] bounds
/// them.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn descendant_file_count(path: &Path) -> usize {
    if !path.is_dir() {
        return 1;
    }
    fn count(dir: &Path, depth: usize) -> usize {
        if depth >= MAX_DEPTH {
            return 0;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut total = 0;
        for entry in read.flatten() {
            let p = entry.path();
            let hidden = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            if p.is_dir() {
                total += count(&p, depth + 1);
            } else {
                total += 1;
            }
        }
        total
    }
    count(path, 0)
}

/// Why an item couldn't be deleted. A dedicated enum for the same reason the
/// other write operations have one — each refusal wants its own words.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum DeleteError {
    Escapes(String),
    /// The path resolved to the workspace root itself.
    IsRoot,
    Io(String),
}

/// Rewrite `path` for an item that has just moved from `from` to `to`.
///
/// Used to keep everything the app is *holding* — the loaded collection, the
/// open report, the set of expanded folders — pointing at the file it was
/// pointing at before, rather than at a path that no longer exists. Matches the
/// moved item itself and anything that was inside it.
pub fn repoint(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    path.strip_prefix(from).ok().map(|rest| to.join(rest))
}

/// Why a new workspace file couldn't be created. Kept apart from a plain string
/// so each front-end can phrase them itself — "already exists" and "resolves
/// outside the workspace" are very different things to tell someone.
#[derive(Clone, Debug, PartialEq)]
pub enum NewItemError {
    /// Nothing was typed (or the dialog was dismissed): not worth reporting.
    EmptyName,
    Escapes(String),
    Exists(String),
    Io(String),
}

/// A path named the way the user thinks of it: relative to the workspace root,
/// since that is the tree they are looking at.
pub fn display_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Whether writing to `target` would physically escape workspace `root` once
/// symlinks are resolved. `target` (a not-yet-created file) is checked via its
/// **deepest existing ancestor**: the closest parent that exists on disk is
/// canonicalised and compared against the canonicalised `root`. A symlinked
/// directory component therefore fails the check even though a lexical `..`
/// scan would pass it, and any subfolders still to be created underneath a
/// real, in-root ancestor are inherently contained. Returns `false` (don't
/// block) when `root` can't be canonicalised — an open workspace root always
/// exists, so that only happens in degenerate cases where the later write will
/// surface the real error.
pub fn escapes_root(root: &Path, target: &Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        return false;
    };
    let mut ancestor = target;
    loop {
        if ancestor.exists() {
            return match ancestor.canonicalize() {
                Ok(real) => !real.starts_with(&canon_root),
                Err(_) => true,
            };
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return true,
        }
    }
}

/// Recursively scans `root`, returning a flattened, depth-first list of
/// entries (each directory immediately followed by its children) — folders
/// sorted before files at each level, then alphabetically within each group.
///
/// When `filter_hurl_json` is `true`, only the workspace's own file types
/// (`.hurl`/`.json` collections, `.vars` environments, `.trail` reports) are
/// included, and any directory whose subtree contains none of those files is
/// omitted entirely (so an unrelated folder full of other file types — images,
/// build artifacts, … — doesn't clutter the tree). When `false`, every
/// non-hidden file is shown and no directory is hidden for being "empty" of
/// matches.
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
        let mut sub = Vec::new();
        scan_dir(&d, depth + 1, filter_hurl_json, &mut sub);
        // Folders are always listed, even when the filter leaves them with
        // nothing inside. The filter chooses which *files* are worth looking at;
        // folders are the structure those files are organised into, and hiding
        // an empty one makes the tree impossible to organise *with* -- a folder
        // created to tidy things into would vanish the moment it was made, and
        // there would be nowhere to drop the first file.
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

/// Whether `path`'s extension marks it as a file the Workspace tree shows: a
/// collection (`.hurl`/`.json`), an environment (`.vars`) or a PaperTrail report
/// (`.trail`) — always `true` when the filter is off. Environments and reports
/// are surfaced so a workspace can hold (and open/run) them alongside the
/// collections they drive; the tree classifies each by extension (see
/// [`is_report_file`] and [`is_env_file`]).
fn is_matching_file(path: &Path, filter_hurl_json: bool) -> bool {
    if !filter_hurl_json {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            ext.eq_ignore_ascii_case("hurl")
                || ext.eq_ignore_ascii_case("json")
                || ext.eq_ignore_ascii_case("vars")
                || ext.eq_ignore_ascii_case("trail")
        }
        None => false,
    }
}

/// Whether `path` is a PaperTrail report (`.trail`, case-insensitive). The
/// Workspace tree uses this to tell a report file apart from a collection file
/// (both are surfaced by [`is_matching_file`]), so selecting one opens the
/// report view rather than trying to parse it as a collection.
pub fn is_report_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("trail"))
}

/// Whether `path` is an environment file. A `.vars` file always is; a `.json`
/// is one only if its contents are a Postman environment export, since Postman
/// writes collections and environments to the same extension and only the
/// content tells them apart. The Workspace tree uses this to tell an
/// environment from a collection file (both are surfaced by
/// [`is_matching_file`]), so selecting one opens it as a global environment
/// rather than trying to parse it as a collection.
///
/// The `.json` answer is memoised: this is called for every row of every
/// workspace redraw, and re-reading a folder of exports each frame would make
/// scrolling the tree cost a full directory's worth of file reads. The cache
/// key includes the file's length and modification time, so editing a file
/// still re-classifies it.
pub fn is_env_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if ext.eq_ignore_ascii_case("vars") {
        return true;
    }
    if !ext.eq_ignore_ascii_case("json") {
        return false;
    }
    is_postman_env_json(path)
}

/// Cached "is this `.json` a Postman environment export?" keyed by path, with
/// the file's `(len, modified)` stamp so an edited file isn't answered from a
/// stale entry.
type JsonEnvCache = HashMap<PathBuf, ((u64, Option<SystemTime>), bool)>;
static JSON_ENV_CACHE: LazyLock<Mutex<JsonEnvCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn is_postman_env_json(path: &Path) -> bool {
    let stamp = std::fs::metadata(path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap_or((0, None));
    // A poisoned cache mutex must not take the UI down over a memo: fall back
    // to reading the file directly.
    if let Ok(cache) = JSON_ENV_CACHE.lock()
        && let Some((cached_stamp, answer)) = cache.get(path)
        && *cached_stamp == stamp
    {
        return *answer;
    }
    let answer = std::fs::read_to_string(path)
        .ok()
        .is_some_and(|c| crate::postman::postman_env_values(&c).is_some());
    if let Ok(mut cache) = JSON_ENV_CACHE.lock() {
        cache.insert(path.to_path_buf(), (stamp, answer));
    }
    answer
}

/// Whether `path` is any file the Workspace tree surfaces — a collection
/// (`.hurl`/`.json`), an environment (`.vars`) or a report (`.trail`). Exposed
/// so the new-report folder chooser can *show* the same files (alongside
/// folders) even though only folders are selectable there: seeing the
/// workspace's own files makes it obvious the picker is scoped inside the
/// workspace rather than browsing the wider filesystem.
pub fn is_workspace_file(path: &Path) -> bool {
    is_matching_file(path, true)
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

    /// Each kind of new file lands where it was asked for, gets its extension
    /// filled in, and starts out as something the format can actually read.
    #[test]
    fn a_new_workspace_file_is_created_with_its_extension_and_valid_starting_content() {
        let root = tmp_dir("new_item");
        fs::create_dir_all(root.join("apis")).expect("subfolder");

        // Named without an extension, and into a subfolder of the root.
        let c = create_item(
            &root,
            &root.join("apis"),
            "billing",
            NewItemKind::Collection,
        )
        .expect("the collection is created");
        assert_eq!(c, root.join("apis/billing.hurl"), "extension filled in");

        let r = create_item(&root, &root, "nightly.trail", NewItemKind::Report)
            .expect("the report is created");
        let text = fs::read_to_string(&r).expect("readable");
        assert!(
            crate::report::parser::parse_flow(&text).is_ok(),
            "a new report parses, rather than starting life broken: {text:?}"
        );

        let e = create_item(&root, &root, "dev", NewItemKind::Environment)
            .expect("the environment is created");
        assert_eq!(e, root.join("dev.vars"));
        assert!(
            fs::read_to_string(&e).expect("readable").starts_with('#'),
            "a new environment is a comment, not an empty file"
        );

        // Subfolders named in passing are created rather than failing.
        let nested = create_item(&root, &root, "team/smoke", NewItemKind::Collection)
            .expect("the nested collection is created");
        assert!(nested.exists(), "the missing folder was created for it");

        // All of them show up in the tree they were added to.
        let names: Vec<String> = scan_workspace(&root, true)
            .into_iter()
            .map(|e| e.display_name)
            .collect();
        for want in ["billing.hurl", "nightly.trail", "dev.vars", "smoke.hurl"] {
            assert!(names.contains(&want.to_string()), "{want} is in the tree");
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// A workspace is shared as a unit, so nothing may be created outside it —
    /// and nothing may quietly replace what's already there.
    #[test]
    fn creating_a_workspace_file_refuses_to_escape_the_root_or_overwrite() {
        let root = tmp_dir("new_item_guard");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("taken.hurl"), "GET https://x\n").expect("existing file");

        assert!(
            matches!(
                create_item(&root, &root, "../outside.hurl", NewItemKind::Collection),
                Err(NewItemError::Escapes(_))
            ),
            "a `..` segment is refused"
        );
        assert!(
            matches!(
                create_item(&root, &root, "/tmp/outside.hurl", NewItemKind::Collection),
                Err(NewItemError::Escapes(_))
            ),
            "an absolute path is refused"
        );
        assert!(
            matches!(
                create_item(&root, &root, "taken.hurl", NewItemKind::Collection),
                Err(NewItemError::Exists(_))
            ),
            "an existing file is never overwritten"
        );
        assert_eq!(
            fs::read_to_string(root.join("taken.hurl")).expect("still there"),
            "GET https://x\n",
            "and it is left exactly as it was"
        );
        assert!(
            matches!(
                create_item(&root, &root, "   ", NewItemKind::Collection),
                Err(NewItemError::EmptyName)
            ),
            "an empty name is nothing to report, just nothing to do"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Dropping a file on a folder moves it there, and everything the app was
    /// holding it by moves with it.
    #[test]
    fn moving_a_workspace_item_relocates_it_and_repoints_what_referred_to_it() {
        let root = tmp_dir("move_item");
        fs::create_dir_all(root.join("apis")).expect("subfolder");
        fs::write(root.join("billing.hurl"), "GET https://x\n").expect("collection");

        let src = root.join("billing.hurl");
        let moved = move_item(&root, &src, &root.join("apis")).expect("it moves");
        assert_eq!(moved, root.join("apis/billing.hurl"));
        assert!(!src.exists(), "it is no longer where it was");
        assert!(moved.exists(), "and it is where it was put");

        // Anything pointing at it (or into it) follows.
        assert_eq!(
            repoint(&src, &src, &moved),
            Some(moved.clone()),
            "the item itself is repointed"
        );
        assert_eq!(
            repoint(
                &root.join("team/a.hurl"),
                &root.join("team"),
                &root.join("apis/team")
            ),
            Some(root.join("apis/team/a.hurl")),
            "and so is anything that was inside a moved folder"
        );
        assert_eq!(
            repoint(&root.join("other.hurl"), &src, &moved),
            None,
            "anything unrelated is left alone"
        );

        // Dropping it back on the folder it is already in does nothing.
        assert_eq!(
            move_item(&root, &moved, &root.join("apis")).expect("no-op"),
            moved,
            "a move to where it already is is not an error"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The same containment rules as creating: nothing leaves the workspace,
    /// nothing is silently replaced, and a folder can't swallow itself.
    #[test]
    fn moving_a_workspace_item_refuses_to_escape_overwrite_or_nest_inside_itself() {
        let root = tmp_dir("move_guard");
        fs::create_dir_all(root.join("apis/deep")).expect("subfolders");
        fs::write(root.join("a.hurl"), "GET https://x\n").expect("collection");
        fs::write(root.join("apis/a.hurl"), "GET https://y\n").expect("clash");

        assert!(
            matches!(
                move_item(&root, &root.join("a.hurl"), &root.join("apis")),
                Err(MoveError::Exists(_))
            ),
            "it will not replace the file already called that"
        );
        assert_eq!(
            fs::read_to_string(root.join("apis/a.hurl")).expect("still there"),
            "GET https://y\n",
            "and the file it would have replaced is untouched"
        );
        assert!(
            matches!(
                move_item(&root, &root.join("apis"), &root.join("apis/deep")),
                Err(MoveError::IntoItself)
            ),
            "a folder cannot be moved inside itself"
        );
        assert!(
            matches!(
                move_item(&root, &root.join("a.hurl"), Path::new("/tmp")),
                Err(MoveError::Escapes(_))
            ),
            "nothing may be moved out of the workspace"
        );
        assert!(
            root.join("a.hurl").exists(),
            "and every refusal leaves the original exactly where it was"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Renaming a file gives it the typed name but keeps its kind: the
    /// extension is regained when omitted, and can't be dropped or swapped for
    /// one that would take the file out of the tree.
    #[test]
    fn renaming_a_file_keeps_the_extension_that_says_what_it_is() {
        let root = tmp_dir("rename_ext");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("billing.hurl"), "GET https://x\n").expect("collection");
        fs::write(root.join("nightly.trail"), "{\"nodes\":[]}\n").expect("report");

        // A name with no extension regains the source's.
        let renamed = rename_item(&root, &root.join("billing.hurl"), "invoices")
            .expect("the collection is renamed");
        assert_eq!(renamed, root.join("invoices.hurl"), "extension regained");
        assert!(!root.join("billing.hurl").exists(), "old name is gone");
        assert!(renamed.exists(), "new name is on disk");

        // A name ending in some *other* extension keeps the report a report
        // rather than letting it vanish from the tree.
        let kept = rename_item(&root, &root.join("nightly.trail"), "archive.bak")
            .expect("the report is renamed");
        assert_eq!(
            kept,
            root.join("archive.bak.trail"),
            "the .trail is preserved so the report stays visible"
        );
        assert!(
            is_report_file(&kept),
            "and it is still recognised as a report"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A folder takes the typed name verbatim, and renaming to the name it
    /// already has is a no-op success rather than a self-collision error.
    #[test]
    fn renaming_a_folder_uses_the_name_verbatim_and_a_same_name_rename_is_a_no_op() {
        let root = tmp_dir("rename_folder");
        fs::create_dir_all(root.join("apis")).expect("subfolder");
        fs::write(root.join("apis/a.hurl"), "GET https://x\n").expect("file inside");

        let renamed =
            rename_item(&root, &root.join("apis"), "services").expect("the folder is renamed");
        assert_eq!(renamed, root.join("services"), "no extension is added");
        assert!(
            root.join("services/a.hurl").exists(),
            "its contents came with it"
        );

        // Same name: nothing changes, and it is not reported as a clash.
        let same = rename_item(&root, &root.join("services"), "services")
            .expect("a same-name rename is a no-op success");
        assert_eq!(same, root.join("services"));

        let _ = fs::remove_dir_all(&root);
    }

    /// The rename guards: a name that already exists is never clobbered, and a
    /// name that is really a path (a separator or a `..`) is refused rather than
    /// used to move the item somewhere else.
    #[test]
    fn renaming_refuses_a_collision_or_a_name_that_is_really_a_path() {
        let root = tmp_dir("rename_guard");
        fs::write(root.join("a.hurl"), "GET https://x\n").expect("collection");
        fs::write(root.join("b.hurl"), "GET https://y\n").expect("other collection");

        assert!(
            matches!(
                rename_item(&root, &root.join("a.hurl"), "b.hurl"),
                Err(RenameError::Exists(_))
            ),
            "a rename onto an existing name is refused"
        );
        assert_eq!(
            fs::read_to_string(root.join("b.hurl")).expect("still there"),
            "GET https://y\n",
            "and the file it would have replaced is untouched"
        );
        for path_like in ["../escape", "sub/nested", "/etc/passwd"] {
            assert!(
                matches!(
                    rename_item(&root, &root.join("a.hurl"), path_like),
                    Err(RenameError::Escapes(_))
                ),
                "a name containing a path ({path_like}) is refused"
            );
        }
        assert!(
            root.join("a.hurl").exists(),
            "every refusal leaves the original exactly where it was"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Deleting a file removes just it; deleting a folder takes its whole
    /// subtree with it, and the count reported beforehand says how much.
    #[test]
    fn deleting_removes_a_file_or_a_whole_folder() {
        let root = tmp_dir("delete_ok");
        fs::create_dir_all(root.join("apis/deep")).expect("subfolders");
        fs::write(root.join("keep.hurl"), "GET https://x\n").expect("survivor");
        fs::write(root.join("apis/a.hurl"), "GET https://a\n").expect("nested file");
        fs::write(root.join("apis/deep/b.hurl"), "GET https://b\n").expect("deeper file");

        assert_eq!(
            descendant_file_count(&root.join("apis")),
            2,
            "the folder holds two files, at any depth"
        );
        assert_eq!(
            descendant_file_count(&root.join("keep.hurl")),
            1,
            "a file counts as one"
        );

        delete_item(&root, &root.join("keep.hurl")).expect("the file is deleted");
        assert!(!root.join("keep.hurl").exists(), "the file is gone");

        delete_item(&root, &root.join("apis")).expect("the folder is deleted");
        assert!(
            !root.join("apis").exists(),
            "the folder and everything under it is gone"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Delete refuses to reach outside the workspace, and refuses to delete the
    /// workspace root itself — "delete this item" must never become "delete
    /// everything".
    #[test]
    fn deleting_refuses_to_escape_the_root_or_delete_the_root() {
        let root = tmp_dir("delete_guard");
        fs::write(root.join("a.hurl"), "GET https://x\n").expect("collection");

        assert!(
            matches!(
                delete_item(&root, Path::new("/tmp")),
                Err(DeleteError::Escapes(_))
            ),
            "nothing outside the workspace may be deleted"
        );
        assert!(
            matches!(delete_item(&root, &root), Err(DeleteError::IsRoot)),
            "the workspace root itself is never deleted"
        );
        assert!(
            root.join("a.hurl").exists() && root.exists(),
            "and every refusal leaves the workspace untouched"
        );

        let _ = fs::remove_dir_all(&root);
    }

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
        fs::write(root.join("env.vars"), "").unwrap();
        fs::write(root.join("run.trail"), "").unwrap();
        fs::write(root.join("skip.txt"), "").unwrap();
        fs::write(root.join("skip.png"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert!(names.contains(&"keep.hurl"));
        assert!(names.contains(&"KEEP.JSON"));
        assert!(names.contains(&"env.vars"));
        assert!(names.contains(&"run.trail"));
        assert!(!names.contains(&"skip.txt"));
        assert!(!names.contains(&"skip.png"));

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

    /// The filter hides *files*, never folders: an empty folder, or one holding
    /// only files the filter rejects, is still somewhere the user can put a
    /// collection, and a freshly created folder would otherwise disappear
    /// before anything could be moved into it.
    #[test]
    fn filter_on_still_lists_folders_that_hold_nothing_it_matches() {
        let root = tmp_dir("hide_empty");
        fs::create_dir_all(root.join("irrelevant")).unwrap();
        fs::write(root.join("irrelevant/notes.txt"), "").unwrap();
        fs::create_dir_all(root.join("brand_new")).unwrap();
        fs::create_dir_all(root.join("relevant")).unwrap();
        fs::write(root.join("relevant/req.hurl"), "").unwrap();

        let entries = scan_workspace(&root, true);
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert!(
            names.contains(&"brand_new"),
            "a folder just created to organise into is still shown when filtered"
        );
        assert!(
            names.contains(&"irrelevant"),
            "and so is one whose only contents the filter rejects"
        );
        assert!(
            !names.contains(&"notes.txt"),
            "the rejected file itself stays hidden -- the filter still applies to files"
        );
        assert!(names.contains(&"relevant"));
        assert!(names.contains(&"req.hurl"));

        let _ = fs::remove_dir_all(&root);
    }

    /// A new folder is made as a directory, not as a file with a folder-ish
    /// name: it must gain no extension and hold no starter content.
    #[test]
    fn creating_a_folder_makes_a_directory_and_never_appends_an_extension() {
        let root = tmp_dir("new_folder");

        let made = create_item(&root, &root, "v2 endpoints", NewItemKind::Folder)
            .expect("a plain name inside the root is allowed");
        assert!(made.is_dir(), "a folder was created, not a file");
        assert_eq!(
            made.file_name().unwrap(),
            "v2 endpoints",
            "the name is exactly what was typed -- no .hurl was appended"
        );

        // A name that already contains a dot keeps it: that is a folder name,
        // not an extension to be reasoned about.
        let dotted = create_item(&root, &root, "v1.2", NewItemKind::Folder).unwrap();
        assert_eq!(dotted.file_name().unwrap(), "v1.2");

        // And the containment checks still apply.
        assert!(
            matches!(
                create_item(&root, &root, "../escape", NewItemKind::Folder),
                Err(NewItemError::Escapes(_))
            ),
            "a folder cannot be created outside the workspace"
        );
        assert!(
            matches!(
                create_item(&root, &root, "v2 endpoints", NewItemKind::Folder),
                Err(NewItemError::Exists(_))
            ),
            "and an existing folder is not silently reused"
        );

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
