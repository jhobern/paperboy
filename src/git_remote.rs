//! Loading collections/environments from a remote git repository without
//! cloning the whole thing.
//!
//! The flow (mirrored by the TUI wizard) is:
//!   1. `list_refs`   — `git ls-remote` to list branches + tags (no clone).
//!   2. `list_files`  — init a throwaway repo, do a **blobless** shallow fetch
//!      of the chosen ref (`--filter=blob:none --depth 1`, so no file contents
//!      are downloaded), then `git ls-tree` to enumerate every path.
//!   3. `checkout_file` — `git checkout FETCH_HEAD -- <path>` materialises
//!      **only** the one file the user picked (its blob is lazily fetched); no
//!      other file is ever written to disk.
//!
//! An optional access token is injected into https(s) remotes as
//! `x-access-token:<token>@host` (GitHub style). If no token is given the URL
//! is used as-is, so public repositories work with no credentials.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Branches and tags advertised by a remote.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RemoteRefs {
    pub branches: Vec<String>,
    pub tags: Vec<String>,
}

/// Whether a [`GitOrigin`] ref is a branch or a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    Branch,
    Tag,
}

/// Where a collection's `.hurl` file (or an environment's `.vars` file) was
/// loaded from in git: which repo, which branch/tag, and its path within the
/// repo. Remembered so the tab title / Environment heading can show the git
/// icon, and so "Save to Git" can default its fields. Never holds a token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitOrigin {
    pub repo_url: String,
    /// Path of the file within the repo, e.g. `collections/foo.hurl`.
    pub path: String,
    pub ref_kind: RefKind,
    /// Bare branch or tag name (no `refs/heads/`/`refs/tags/` prefix).
    pub ref_name: String,
}

impl GitOrigin {
    /// The full ref path, e.g. `refs/heads/main` or `refs/tags/v1.0`.
    pub fn gitref(&self) -> String {
        match self.ref_kind {
            RefKind::Branch => branch_ref(&self.ref_name),
            RefKind::Tag => tag_ref(&self.ref_name),
        }
    }
}

/// Split a full ref path (e.g. `refs/heads/main` / `refs/tags/v1.0`) back into
/// its [`RefKind`] and bare name. Anything not matching one of those two
/// prefixes is treated as a branch name as-is (defensive fallback; the ref
/// strings passed in always come from [`branch_ref`]/[`tag_ref`]).
pub fn parse_ref_kind(gitref: &str) -> (RefKind, String) {
    if let Some(name) = gitref.strip_prefix("refs/heads/") {
        (RefKind::Branch, name.to_string())
    } else if let Some(name) = gitref.strip_prefix("refs/tags/") {
        (RefKind::Tag, name.to_string())
    } else {
        (RefKind::Branch, gitref.to_string())
    }
}

/// Inject a token into an http(s) URL as `x-access-token:<token>@host`.
/// Non-http URLs, a missing/blank token, or URLs that already embed
/// credentials are returned unchanged.
fn authed_url(url: &str, token: Option<&str>) -> String {
    let url = url.trim();
    let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return url.to_string();
    };
    for scheme in ["https://", "http://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            // Respect credentials the user already put in the URL.
            if rest
                .split('/')
                .next()
                .is_some_and(|host| host.contains('@'))
            {
                return url.to_string();
            }
            return format!("{scheme}x-access-token:{token}@{rest}");
        }
    }
    url.to_string()
}

/// Replace the token with `***` in a message so it is never surfaced to the UI.
fn redact(msg: &str, token: Option<&str>) -> String {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => msg.replace(t, "***"),
        None => msg.to_string(),
    }
}

/// Run a git command, returning stdout on success or a redacted stderr on
/// failure. `GIT_TERMINAL_PROMPT=0` stops git from blocking on a credential
/// prompt (which would hang the whole app).
fn run_git(args: &[&str], cwd: Option<&Path>, token: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_ASKPASS", "echo");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run git (is it installed?): {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let err = if err.is_empty() {
            "git command failed"
        } else {
            err
        };
        Err(redact(err, token))
    }
}

/// Parse `git ls-remote --heads --tags` output into branch and tag lists.
fn parse_refs(out: &str) -> RemoteRefs {
    let mut refs = RemoteRefs::default();
    for line in out.lines() {
        let Some((_sha, name)) = line.split_once('\t') else {
            continue;
        };
        if let Some(b) = name.strip_prefix("refs/heads/") {
            refs.branches.push(b.to_string());
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            // Skip the peeled `^{}` entries annotated tags produce.
            if !t.ends_with("^{}") {
                refs.tags.push(t.to_string());
            }
        }
    }
    refs.branches.sort();
    refs.tags.sort();
    refs
}

/// List the branches and tags of a remote without cloning it.
pub fn list_refs(url: &str, token: Option<&str>) -> Result<RemoteRefs, String> {
    let au = authed_url(url, token);
    let out = run_git(&["ls-remote", "--heads", "--tags", &au], None, token)?;
    let refs = parse_refs(&out);
    if refs.branches.is_empty() && refs.tags.is_empty() {
        return Err("No branches or tags found (check the URL and token).".to_string());
    }
    Ok(refs)
}

/// Full ref path for a branch, e.g. `refs/heads/main`.
pub fn branch_ref(name: &str) -> String {
    format!("refs/heads/{name}")
}

/// Full ref path for a tag, e.g. `refs/tags/v1.0`.
pub fn tag_ref(name: &str) -> String {
    format!("refs/tags/{name}")
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn make_temp_repo() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "paperboy-git-{}-{}-{}",
        std::process::id(),
        nanos,
        seq
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create temp dir: {e}"))?;
    Ok(dir)
}

/// Best-effort removal of a temp repo created by [`list_files`].
pub fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo);
}

/// Best-effort removal of the `origin` remote from `repo` so its `.git/config`
/// no longer stores the authed fetch URL (which embeds the access token). Used
/// when a temp repo outlives the load — a git-loaded workspace keeps its temp
/// repo as the persisted `workspace_root`, so the token must not linger there.
/// Downloads are complete by the time this runs, so dropping the remote is
/// harmless. Errors are ignored: at worst the config is left as-is.
pub fn scrub_remote(repo: &Path) {
    let _ = run_git(&["remote", "remove", "origin"], Some(repo), None);
}

/// Blobless-fetch the given ref into a throwaway repo and list every file path
/// at that ref. Returns the paths, the resolved commit sha (so a later
/// redownload can pin the exact same commit rather than "whatever the branch
/// points at now"), and the temp repo (kept alive so the chosen file can be
/// checked out from it without re-fetching). The caller must call [`cleanup`]
/// on the returned path when done or on cancel.
pub fn list_files(
    url: &str,
    token: Option<&str>,
    gitref: &str,
) -> Result<(Vec<String>, PathBuf, String), String> {
    let au = authed_url(url, token);
    let dir = make_temp_repo()?;

    let result = (|| {
        run_git(&["init", "-q"], Some(&dir), token)?;
        run_git(&["remote", "add", "origin", &au], Some(&dir), token)?;
        // No blobs, shallow: we only need the tree to list paths.
        run_git(
            &[
                "fetch",
                "-q",
                "--filter=blob:none",
                "--depth",
                "1",
                "origin",
                gitref,
            ],
            Some(&dir),
            token,
        )?;
        let out = run_git(
            &["ls-tree", "-r", "--name-only", "FETCH_HEAD"],
            Some(&dir),
            token,
        )?;
        let files: Vec<String> = out
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        if files.is_empty() {
            return Err("No files found at that branch/tag.".to_string());
        }
        let sha = run_git(&["rev-parse", "FETCH_HEAD"], Some(&dir), token)?;
        Ok((files, sha.trim().to_string()))
    })();

    match result {
        Ok((files, sha)) => Ok((files, dir, sha)),
        Err(e) => {
            cleanup(&dir);
            Err(e)
        }
    }
}

/// Check out **only** `path` from the already-fetched ref and return its
/// contents. The single blob is lazily fetched from the promisor remote; no
/// other file is written.
pub fn checkout_file(repo: &Path, path: &str) -> Result<String, String> {
    run_git(&["checkout", "FETCH_HEAD", "--", path], Some(repo), None)?;
    std::fs::read_to_string(repo.join(path))
        .map_err(|e| format!("could not read the checked-out file: {e}"))
}

/// Check out **only** `paths` from the already-fetched ref, writing them all
/// to disk under `repo` — used by the Workspace git-load flow, which needs a
/// filtered batch of files rather than the single one `checkout_file`
/// handles. Every other blob in the tree is left unfetched (same "no full
/// checkout" philosophy as `checkout_file`). Paths are checked out in
/// chunks so a workspace with a very large number of matching files never
/// blows past the OS's command-line length limit.
pub fn checkout_files(repo: &Path, paths: &[String]) -> Result<(), String> {
    for chunk in paths.chunks(500) {
        let mut args: Vec<&str> = vec!["checkout", "FETCH_HEAD", "--"];
        args.extend(chunk.iter().map(String::as_str));
        run_git(&args, Some(repo), None)?;
    }
    Ok(())
}

// ── Save-to-git (push) plumbing ─────────────────────────────────────────────
//
// Mirrors the "no full checkout" philosophy above: to build a new commit on
// top of an existing (possibly huge) repo we never materialise its whole
// tree. Instead:
//   1. `fetch_base`   — blobless depth-1 fetch of just the base ref, giving us
//      its tip commit sha in a throwaway repo (same trick as `list_files`).
//   2. `commit_files` — `git read-tree <base>` stages the *entire* parent tree
//      into the index without touching the working directory (tree objects
//      only, no blobs), then only the handful of files actually being saved
//      are written to disk and `git add`-ed, `write-tree`, `commit-tree`. No
//      other blob is ever fetched or written.
//   3. `push_commit`  — push the new commit straight to a ref by SHA, never
//      with `--force`; a rejected (non-fast-forward) push surfaces as a plain
//      error, with no retry/merge/rebase attempted.

/// Blobless-fetch `gitref` (a branch or tag's full ref, e.g.
/// `refs/heads/main`) into a throwaway repo and return the repo path plus its
/// tip commit sha. The caller must [`cleanup`] the returned path when done.
pub fn fetch_base(
    url: &str,
    token: Option<&str>,
    gitref: &str,
) -> Result<(PathBuf, String), String> {
    let au = authed_url(url, token);
    let dir = make_temp_repo()?;
    let result = (|| {
        run_git(&["init", "-q"], Some(&dir), token)?;
        run_git(&["remote", "add", "origin", &au], Some(&dir), token)?;
        run_git(
            &[
                "fetch",
                "-q",
                "--filter=blob:none",
                "--depth",
                "1",
                "origin",
                gitref,
            ],
            Some(&dir),
            token,
        )?;
        let sha = run_git(&["rev-parse", "FETCH_HEAD"], Some(&dir), token)?;
        Ok(sha.trim().to_string())
    })();
    match result {
        Ok(sha) => Ok((dir, sha)),
        Err(e) => {
            cleanup(&dir);
            Err(e)
        }
    }
}

/// Query the remote's current branches/tags, always **freshly** (no cached
/// state), so an existence check can't be fooled by another user having
/// pushed a same-named branch/tag since this app last looked.
pub fn refs_fresh(url: &str, token: Option<&str>) -> Result<RemoteRefs, String> {
    let au = authed_url(url, token);
    let out = run_git(&["ls-remote", "--heads", "--tags", &au], None, token)?;
    Ok(parse_refs(&out))
}

/// Build a new commit in `repo` on top of `base_sha`, writing/replacing only
/// `files` (repo-relative path -> full new content) — every other path in the
/// parent tree is carried over untouched via `read-tree`, without ever being
/// checked out to disk. Returns the new commit's sha.
pub fn commit_files(
    repo: &Path,
    base_sha: &str,
    files: &[(String, String)],
    message: &str,
    author_name: &str,
    author_email: &str,
) -> Result<String, String> {
    // Stage the parent commit's whole tree into the index (tree objects only;
    // no blob content is fetched or written for files we're not touching).
    run_git(&["read-tree", base_sha], Some(repo), None)?;
    for (path, content) in files {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| format!("could not write {}: {e}", full.display()))?;
        run_git(&["add", "--", path], Some(repo), None)?;
    }
    let tree = run_git(&["write-tree"], Some(repo), None)?;
    let tree = tree.trim();
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GIT_AUTHOR_NAME", author_name)
        .env("GIT_AUTHOR_EMAIL", author_email)
        .env("GIT_COMMITTER_NAME", author_name)
        .env("GIT_COMMITTER_EMAIL", author_email)
        .args(["commit-tree", tree, "-p", base_sha, "-m", message]);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run git (is it installed?): {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            "git commit-tree failed".to_string()
        } else {
            err.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Push `commit_sha` directly to `target_ref` (e.g. `refs/heads/foo` or
/// `refs/tags/bar`) on the remote. **Never** forces — a non-fast-forward or
/// already-existing-tag rejection is returned as a plain error, with no
/// retry, merge, or rebase attempted.
pub fn push_commit(
    url: &str,
    token: Option<&str>,
    repo: &Path,
    commit_sha: &str,
    target_ref: &str,
) -> Result<(), String> {
    let au = authed_url(url, token);
    let refspec = format!("{commit_sha}:{target_ref}");
    run_git(&["push", &au, &refspec], Some(repo), token)?;
    Ok(())
}

/// Fetch git's configured author identity (`user.name` / `user.email`),
/// falling back to a generic identity if either is unset so a commit can
/// always be made even on a machine with no git config.
pub fn author_identity() -> (String, String) {
    let name = run_git(&["config", "user.name"], None, None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let email = run_git(&["config", "user.email"], None, None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (
        name.unwrap_or_else(|| "PaperBoy".to_string()),
        email.unwrap_or_else(|| "paperboy@localhost".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_token_into_https_urls() {
        assert_eq!(
            authed_url("https://github.com/o/r.git", Some("TKN")),
            "https://x-access-token:TKN@github.com/o/r.git"
        );
    }

    #[test]
    fn leaves_url_untouched_without_a_token() {
        assert_eq!(
            authed_url("https://github.com/o/r.git", None),
            "https://github.com/o/r.git"
        );
        assert_eq!(
            authed_url("https://github.com/o/r.git", Some("   ")),
            "https://github.com/o/r.git"
        );
    }

    #[test]
    fn does_not_double_up_existing_credentials() {
        assert_eq!(
            authed_url("https://user@github.com/o/r.git", Some("TKN")),
            "https://user@github.com/o/r.git"
        );
    }

    #[test]
    fn leaves_ssh_urls_untouched() {
        assert_eq!(
            authed_url("git@github.com:o/r.git", Some("TKN")),
            "git@github.com:o/r.git"
        );
    }

    #[test]
    fn redacts_the_token_from_messages() {
        assert_eq!(
            redact("fatal: bad creds for SECRET", Some("SECRET")),
            "fatal: bad creds for ***"
        );
        assert_eq!(redact("no token here", None), "no token here");
    }

    #[test]
    fn parses_branches_and_tags_and_skips_peeled() {
        let out = "\
abc123\trefs/heads/main
def456\trefs/heads/dev
1111\trefs/tags/v1.0
2222\trefs/tags/v1.0^{}
3333\tHEAD
";
        let refs = parse_refs(out);
        assert_eq!(refs.branches, vec!["dev", "main"]);
        assert_eq!(refs.tags, vec!["v1.0"]);
    }

    #[test]
    fn parses_ref_kind_from_full_ref_paths() {
        assert_eq!(
            parse_ref_kind("refs/heads/main"),
            (RefKind::Branch, "main".to_string())
        );
        assert_eq!(
            parse_ref_kind("refs/tags/v1.0"),
            (RefKind::Tag, "v1.0".to_string())
        );
    }

    // ── Local-bare-repo integration tests for the save/push plumbing ───────
    //
    // No network needed: git treats a filesystem path as a perfectly normal
    // remote, so these fixtures exercise the real `git fetch`/`push` code
    // paths end-to-end while staying fully offline and fast.

    fn git(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Build a bare "remote" repo seeded with one commit on `main` containing
    /// a small collection file plus a deliberately large unrelated blob, so
    /// tests can assert that blob is never fetched/written by our plumbing.
    /// Returns the bare repo path.
    fn seed_bare_repo() -> PathBuf {
        let base = make_temp_repo().unwrap();
        let bare = base.join("bare.git");
        let work = base.join("work");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&["init", "--bare", "-q", "."], &bare);
        git(&["init", "-q"], &work);
        git(&["checkout", "-q", "-b", "main"], &work);
        git(&["config", "user.name", "Seed"], &work);
        git(&["config", "user.email", "seed@test"], &work);
        std::fs::create_dir_all(work.join("collections")).unwrap();
        std::fs::write(
            work.join("collections/foo.hurl"),
            "GET https://example.com\n",
        )
        .unwrap();
        std::fs::create_dir_all(work.join("big")).unwrap();
        std::fs::write(work.join("big/blob.bin"), "x".repeat(1024)).unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "-q", "-m", "seed"], &work);
        git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
        git(&["push", "-q", "origin", "main"], &work);
        bare
    }

    #[test]
    fn checkout_files_downloads_only_the_requested_paths_and_skips_everything_else() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (files, repo, _sha) = list_files(bare_url, None, "refs/heads/main").unwrap();
        assert!(files.contains(&"collections/foo.hurl".to_string()));
        assert!(files.contains(&"big/blob.bin".to_string()));

        checkout_files(&repo, &["collections/foo.hurl".to_string()]).unwrap();

        assert!(
            repo.join("collections/foo.hurl").exists(),
            "the requested file is checked out"
        );
        assert!(
            !repo.join("big/blob.bin").exists(),
            "a file not in the requested set is never checked out, even though it's in the same tree"
        );

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn scrub_remote_removes_the_origin_so_no_token_is_left_in_the_config() {
        // A token-authed https load writes the authed URL (token embedded)
        // into the temp repo's origin remote; a persisted workspace keeps
        // that repo, so the token would linger in `.git/config` without
        // scrubbing.
        let repo = make_temp_repo().unwrap();
        git(&["init", "-q"], &repo);
        let authed = authed_url("https://github.com/o/r.git", Some("SECRET"));
        git(&["remote", "add", "origin", &authed], &repo);

        let config = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        assert!(
            config.contains("SECRET"),
            "sanity: the authed URL (with the token) is initially in the config"
        );

        scrub_remote(&repo);

        let config = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        assert!(
            !config.contains("SECRET"),
            "scrub_remote drops the origin remote so the token no longer sits on disk"
        );
        assert!(
            !config.contains("origin"),
            "the origin remote is gone entirely"
        );

        cleanup(&repo);
    }

    #[test]
    fn checkout_files_can_check_out_several_paths_at_once() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (_files, repo, _sha) = list_files(bare_url, None, "refs/heads/main").unwrap();
        checkout_files(
            &repo,
            &[
                "collections/foo.hurl".to_string(),
                "big/blob.bin".to_string(),
            ],
        )
        .unwrap();

        assert!(repo.join("collections/foo.hurl").exists());
        assert!(repo.join("big/blob.bin").exists());

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn appends_a_commit_to_the_existing_branch_without_checking_out_unrelated_files() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (repo, base_sha) = fetch_base(bare_url, None, "refs/heads/main").unwrap();
        let new_sha = commit_files(
            &repo,
            &base_sha,
            &[(
                "collections/foo.hurl".to_string(),
                "GET https://example.com/v2\n".to_string(),
            )],
            "update via test",
            "Test",
            "test@test",
        )
        .unwrap();
        push_commit(bare_url, None, &repo, &new_sha, "refs/heads/main").unwrap();

        // The unrelated (large) blob was never checked out to disk, only the
        // file we actually wrote.
        assert!(
            !repo.join("big/blob.bin").exists(),
            "unrelated blob should never be checked out"
        );
        assert!(repo.join("collections/foo.hurl").exists());

        // The bare repo now has two commits on main with the new content.
        let log = String::from_utf8(
            Command::new("git")
                .args(["--git-dir", bare_url, "log", "--oneline", "refs/heads/main"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(log.lines().count(), 2);

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn creates_a_brand_new_branch() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (repo, base_sha) = fetch_base(bare_url, None, "refs/heads/main").unwrap();
        let new_sha = commit_files(
            &repo,
            &base_sha,
            &[(
                "collections/foo.hurl".to_string(),
                "GET https://example.com/branch\n".to_string(),
            )],
            "new branch",
            "Test",
            "test@test",
        )
        .unwrap();
        push_commit(bare_url, None, &repo, &new_sha, "refs/heads/feature-x").unwrap();

        let refs = refs_fresh(bare_url, None).unwrap();
        assert!(refs.branches.contains(&"feature-x".to_string()));

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn creates_a_brand_new_tag() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (repo, base_sha) = fetch_base(bare_url, None, "refs/heads/main").unwrap();
        push_commit(bare_url, None, &repo, &base_sha, "refs/tags/v1.0").unwrap();

        let refs = refs_fresh(bare_url, None).unwrap();
        assert!(refs.tags.contains(&"v1.0".to_string()));

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn rejects_reusing_an_existing_tag_name() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        let (repo, base_sha) = fetch_base(bare_url, None, "refs/heads/main").unwrap();
        push_commit(bare_url, None, &repo, &base_sha, "refs/tags/v1.0").unwrap();

        // A second attempt at the same tag name (even with a different
        // commit) must fail — tags are never overwritten, and we never pass
        // --force.
        let new_sha = commit_files(
            &repo,
            &base_sha,
            &[(
                "collections/foo.hurl".to_string(),
                "GET https://example.com/other\n".to_string(),
            )],
            "other",
            "Test",
            "test@test",
        )
        .unwrap();
        let result = push_commit(bare_url, None, &repo, &new_sha, "refs/tags/v1.0");
        assert!(result.is_err(), "re-pushing an existing tag name must fail");

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn rejects_pushing_from_a_stale_branch_tip() {
        let bare = seed_bare_repo();
        let bare_url = bare.to_str().unwrap();

        // Fetch the base first (this is now "stale" once another push lands).
        let (repo, base_sha) = fetch_base(bare_url, None, "refs/heads/main").unwrap();

        // Someone else advances `main` in the meantime.
        push_commit(
            bare_url,
            None,
            &repo,
            &commit_files(
                &repo,
                &base_sha,
                &[(
                    "collections/foo.hurl".to_string(),
                    "GET https://example.com/other-user\n".to_string(),
                )],
                "other user's commit",
                "Other",
                "other@test",
            )
            .unwrap(),
            "refs/heads/main",
        )
        .unwrap();

        // Our own commit, still built on the now-stale `base_sha`, must be
        // rejected (non-fast-forward) — no merge/rebase is attempted.
        let stale_sha = commit_files(
            &repo,
            &base_sha,
            &[(
                "collections/foo.hurl".to_string(),
                "GET https://example.com/mine\n".to_string(),
            )],
            "my stale commit",
            "Test",
            "test@test",
        )
        .unwrap();
        let result = push_commit(bare_url, None, &repo, &stale_sha, "refs/heads/main");
        assert!(
            result.is_err(),
            "pushing from a stale base must fail, not overwrite"
        );

        cleanup(&repo);
        cleanup(bare.parent().unwrap());
    }

    #[test]
    fn author_identity_falls_back_when_config_is_missing() {
        // Just assert the function never panics and always returns non-empty
        // strings; exact values depend on the host's git config.
        let (name, email) = author_identity();
        assert!(!name.is_empty());
        assert!(!email.is_empty());
    }
}
