//! `.vars` environment files: `KEY=value`, where a value may be a `{{ … }}`
//! provider reference resolved at load time:
//!   `{{ env:NAME }}`  from the process environment (`std::env`)
//!   `{{ ssm:/path }}` from AWS SSM Parameter Store (`aws ssm get-parameter`)
//!   `{{ op://… }}`    from 1Password (`op read`)
//!
//! External providers are resolved by shelling out to their CLI. If the CLI is
//! missing, not signed in, or the reference can't be found, the value is kept
//! verbatim and flagged unresolved (shown with a warning in the UI).

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use regex::{Captures, Regex};

/// A `{{ KEY }}` placeholder (inner whitespace ignored). Shared by
/// [`referenced_keys`] and [`substitute`] so both agree on what a placeholder is.
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([^{}]+?)\s*\}\}").unwrap());

/// The source/provider for a variable's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSource {
    Literal,
    ProcessEnv,
    Ssm,
    OnePassword,
    Unknown,
}

/// A single variable from a `.vars` file.
#[derive(Debug, Clone)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
    pub source: ValueSource,
    /// `true` when the value was fully resolved to a concrete string.
    pub resolved: bool,
    /// `true` while an external secret is still being fetched in the background.
    pub loading: bool,
    /// The value as originally loaded (updated once a secret resolves). The
    /// "reset" action in the editor restores this.
    pub original_value: String,
    /// `true` once the user has edited this value away from `original_value`.
    pub modified: bool,
    /// `true` when the user added this variable by hand (rather than it coming
    /// from a loaded `.vars` file). Marked in the UI so its origin is clear; a
    /// later file load with the same key replaces it (clearing this flag).
    pub user_added: bool,
    /// The `.vars` source token for this variable (its provider reference like
    /// `{{ op://… }}`, or a literal value) — i.e. what would be written back to a
    /// `.vars` file. Persisted to session state so environments reload and
    /// re-resolve on startup WITHOUT ever writing a resolved secret to disk.
    pub raw: String,
}

/// Fixed-width mask shown in place of a secret value (does not leak length).
pub const SECRET_MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Flatten a value to something the `.vars` line format can actually hold.
///
/// A `.vars` file is one `KEY=value` per line, and the parser drops any line
/// without an `=`. A value containing a newline — a pasted PEM private key or
/// certificate is the realistic case — therefore came back from disk as its
/// first line alone, losing the rest silently, on save and again on every
/// restart. The format can't represent it, so collapse it here, where the user
/// can see what was stored, instead of appearing to accept it and truncating
/// it later. (The Postman importer has always done the same on the way in.)
pub fn flatten_value(v: &str) -> String {
    if v.contains(['\n', '\r']) {
        v.replace(['\n', '\r'], " ")
    } else {
        v.to_string()
    }
}

impl EnvVar {
    /// A variable added by the user by hand: a concrete literal value that is
    /// immediately "resolved" and flagged [`user_added`](EnvVar::user_added).
    pub fn user(key: String, value: String) -> Self {
        let value = flatten_value(&value);
        EnvVar {
            key,
            original_value: value.clone(),
            raw: value.clone(),
            value,
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            modified: false,
            user_added: true,
        }
    }

    /// A resolved value pulled from an external secret provider (1Password or
    /// SSM). Such values are treated as secret and masked in the UI.
    pub fn is_secret(&self) -> bool {
        self.resolved && matches!(self.source, ValueSource::OnePassword | ValueSource::Ssm)
    }

    /// A provider-backed secret (1Password / SSM), regardless of whether it has
    /// resolved yet. Its value must never be written to the plaintext state file.
    pub fn is_secret_source(&self) -> bool {
        matches!(self.source, ValueSource::OnePassword | ValueSource::Ssm)
    }

    /// A secret-provider reference that has not been resolved to a concrete
    /// value yet (still loading, or the load failed). Requests that reference
    /// such a variable must not be sent until it resolves.
    pub fn is_pending(&self) -> bool {
        matches!(self.source, ValueSource::OnePassword | ValueSource::Ssm) && !self.resolved
    }

    /// A variable that failed to load: an unresolved secret-provider reference
    /// (1Password / SSM) or a `{{ env:NAME }}` that wasn't found in the process
    /// environment. Distinguished from still-`loading` (in flight) and
    /// `resolved` (succeeded) so the UI can offer to retry just this one entry
    /// instead of reloading the whole environment.
    pub fn is_failed(&self) -> bool {
        !self.loading
            && !self.resolved
            && matches!(
                self.source,
                ValueSource::ProcessEnv | ValueSource::OnePassword | ValueSource::Ssm
            )
    }

    /// Re-attempt resolving this variable from its persisted `raw` token (e.g.
    /// `{{ op://… }}` or `{{ env:NAME }}`), for a variable that previously
    /// failed to load. A no-op (returns `None`) unless [`is_failed`](Self::is_failed).
    /// `{{ env:NAME }}` re-resolves immediately (it only reads the process
    /// environment); a provider reference is re-classified as `loading` and
    /// returned as a [`PendingSecret`] for the caller to resolve in the
    /// background via [`spawn_resolution`], exactly like a fresh file load.
    pub fn reload(&mut self, index: usize) -> Option<PendingSecret> {
        if !self.is_failed() {
            return None;
        }
        let c = classify_raw(&self.raw);
        self.source = c.source;
        self.resolved = c.resolved;
        self.loading = c.loading;
        self.value = c.value.clone();
        if !self.modified {
            self.original_value = c.value;
        }
        c.secret.map(|kind| PendingSecret { index, kind })
    }

    /// The value to show in the UI: a fixed-width mask for secrets, otherwise
    /// the raw value (which for an unresolved provider is just its reference).
    pub fn display_value(&self) -> String {
        if self.is_secret() {
            SECRET_MASK.to_string()
        } else {
            self.value.clone()
        }
    }

    /// Record a user edit to a variable that was sourced from a secret provider
    /// (1Password / SSM), with an explicit choice of whether the new value
    /// should still be treated as secret.
    ///
    /// `keep_secret == true` preserves the current behaviour: the edit is kept
    /// only in memory (`value`) and never written to the persisted source
    /// (`raw`), so it is lost on exit rather than leaking into the plaintext
    /// state file.
    ///
    /// `keep_secret == false` means the user has confirmed the new value is no
    /// longer sensitive: it is written to `raw` (so it persists to disk like
    /// any other value).
    ///
    /// For a variable that was never secret-sourced, `keep_secret` has no
    /// effect — its value always persists.
    ///
    /// Whenever the persisted source (`raw`) is updated, its new text is
    /// reclassified exactly as when a `.vars` file is first parsed: a plain
    /// string becomes (or stays) a literal, while text that now looks like a
    /// `{{ op://… }}` or `{{ ssm:… }}` reference turns the variable into that
    /// provider's secret and is returned as a [`PendingSecret`] for the caller
    /// to resolve in the background (`index` is this variable's position,
    /// used to route the resolved value back via [`EnvUpdate`]).
    pub fn set_user_value_secrecy(
        &mut self,
        v: String,
        keep_secret: bool,
        index: usize,
    ) -> Option<PendingSecret> {
        let v = flatten_value(&v);
        self.modified = v != self.original_value;
        let was_secret_source = self.is_secret_source();
        let rewrite_raw = !was_secret_source || !keep_secret;
        self.value = v.clone();
        if !rewrite_raw {
            // Kept in memory only: the previous provider reference (`raw`)
            // is untouched, and the edited value is treated as resolved so it
            // displays (masked) and is used normally until the app exits.
            self.resolved = true;
            self.loading = false;
            return None;
        }
        self.raw = v;
        let c = classify_raw(&self.raw);
        self.source = c.source;
        self.resolved = c.resolved;
        self.loading = c.loading;
        self.value = c.value;
        c.secret.map(|kind| PendingSecret { index, kind })
    }
}

/// A loaded environment (one `.vars` file). Environments now live in a single
/// global list ([`crate::tui::app::TuiApp::global_envs`]) rather than being
/// owned by a single [`crate::collection::Collection`] — a collection instead
/// holds an optional `linked_env_id` referencing one of these by [`id`](Self::id),
/// and any number of collections may link the same environment.
#[derive(Debug, Clone)]
pub struct Environment {
    /// Unique id so background secret updates (and collection links) can be
    /// routed to this instance even as the global list is edited.
    pub id: u64,
    pub name: String,
    pub vars: Vec<EnvVar>,
    /// Source `.vars` file this environment was loaded from (used by "Save
    /// Environment"). `None` for a hand-made or remote environment until saved.
    pub path: Option<std::path::PathBuf>,
    /// Where the `.vars` file was loaded from in git, if it was (used to show
    /// the ⎇ icon in the Global Environments panel).
    pub git_origin: Option<crate::git_remote::GitOrigin>,
}

/// Whether `env` was loaded from the very place a load of `path`/`git_origin`
/// would read — i.e. this is the *same file* being opened again, not a second
/// environment that happens to share a name.
///
/// The distinction matters because the two cases want opposite treatment.
/// Re-opening a file you already have loaded should just refresh it; only a
/// genuine clash between two different sources is a question worth asking (or,
/// in the GUI, worth disambiguating with a `(2)` suffix). Matching on the name
/// alone conflated them, so pressing Enter on a workspace environment that was
/// already loaded raised a four-way prompt in which none of the answers was
/// what the user meant.
///
/// A hand-made environment has no source, so it never matches: there is nothing
/// to re-read it from.
pub fn is_same_source(
    env: &Environment,
    path: Option<&std::path::Path>,
    git_origin: Option<&crate::git_remote::GitOrigin>,
) -> bool {
    if let Some(origin) = git_origin {
        return env.git_origin.as_ref() == Some(origin);
    }
    match (env.path.as_deref(), path) {
        (Some(a), Some(b)) => same_file(a, b),
        _ => false,
    }
}

/// Whether two paths name the same file. Compared literally first so the common
/// case costs nothing, then canonicalised, because the same environment can
/// easily be reached by both a relative and an absolute path (the workspace
/// tree walks relative to its root; File -> Load Environment returns absolute).
/// A path that cannot be canonicalised - deleted since, or not yet written - is
/// only equal to itself.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

impl Environment {
    /// Serialize back to `.vars` source text (`KEY=source` per line), using each
    /// variable's `raw` form so resolved secret values are never written to disk.
    pub fn to_vars_text(&self) -> String {
        self.vars
            .iter()
            .map(|v| format!("{}={}", v.key, v.raw))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Apply a background resolution result to the matching variable.
    pub fn apply_update(&mut self, update: &EnvUpdate) {
        if update.env_id != self.id {
            return;
        }
        if let Some(var) = self.vars.get_mut(update.index) {
            var.loading = false;
            match &update.value {
                Some(v) => {
                    // Don't clobber a value the user edited while it was loading,
                    // but always record the resolved value as the reset target.
                    if !var.modified {
                        var.value = v.clone();
                    }
                    var.original_value = v.clone();
                    var.resolved = true;
                }
                None => var.resolved = false,
            }
        }
    }
}

/// A secret reference awaiting background resolution.
#[derive(Debug, Clone)]
pub enum SecretKind {
    Op(String),
    Ssm(String),
}

/// A pending secret: which variable (by index) and how to resolve it.
#[derive(Debug, Clone)]
pub struct PendingSecret {
    pub index: usize,
    pub kind: SecretKind,
}

/// The result of resolving one pending secret in the background.
#[derive(Debug, Clone)]
pub struct EnvUpdate {
    pub env_id: u64,
    pub index: usize,
    /// `Some(value)` on success, `None` if the provider couldn't resolve it.
    pub value: Option<String>,
}

static NEXT_ENV_ID: AtomicU64 = AtomicU64::new(1);

/// A process-unique id for a freshly-loaded environment.
pub fn next_env_id() -> u64 {
    NEXT_ENV_ID.fetch_add(1, Ordering::Relaxed)
}

/// Resolves external secret references. Abstracted so the CLI shell-out can be
/// swapped for a fake in tests.
pub trait SecretResolver {
    /// Resolve a full `op://…` reference via the 1Password CLI.
    fn resolve_op(&self, reference: &str) -> Option<String>;
    /// Resolve an SSM parameter (the path after `ssm:`) via the AWS CLI.
    fn resolve_ssm(&self, name: &str) -> Option<String>;
    /// Resolve many `op://` references in a single shot. 1Password prompts
    /// for authorization/unlock per `op` process invocation, so resolving
    /// each reference with its own `op read` call bombards the user with a
    /// separate prompt for every 1Password-backed variable in an
    /// environment. The default implementation just resolves one at a time
    /// (fine for simple test resolvers); [`CliResolver`] overrides this to
    /// batch every reference into a single `op inject` call so 1Password
    /// only needs to authorize once for the whole environment.
    fn resolve_op_batch(&self, references: &[String]) -> Vec<Option<String>> {
        references.iter().map(|r| self.resolve_op(r)).collect()
    }
}

/// Production resolver: shells out to the `op` and `aws` CLIs.
pub struct CliResolver;

impl SecretResolver for CliResolver {
    fn resolve_op(&self, reference: &str) -> Option<String> {
        run_cmd("op", &["read", reference])
    }

    fn resolve_ssm(&self, name: &str) -> Option<String> {
        run_cmd(
            "aws",
            &[
                "ssm",
                "get-parameter",
                "--name",
                name,
                "--with-decryption",
                "--query",
                "Parameter.Value",
                "--output",
                "text",
            ],
        )
    }

    fn resolve_op_batch(&self, references: &[String]) -> Vec<Option<String>> {
        if references.len() <= 1 {
            return references.iter().map(|r| self.resolve_op(r)).collect();
        }
        match run_op_inject_batch(references) {
            Some(values) => values,
            // The whole batch failed (e.g. the `op` CLI is missing, or the
            // user declined the single authorization prompt): fall back to
            // resolving one at a time so a single bad reference doesn't take
            // down every other secret in the batch.
            None => references.iter().map(|r| self.resolve_op(r)).collect(),
        }
    }
}

/// Resolve every `op://` reference in `references` with one `op inject` call,
/// so 1Password only asks for authorization/unlock once for the whole batch.
/// Each reference is wrapped in a pair of unique, randomly-generated marker
/// lines so the (possibly multi-line) resolved value can be extracted
/// unambiguously from the combined output, then fed to `op inject` over
/// stdin. Returns `None` if the CLI itself couldn't be run at all.
fn run_op_inject_batch(references: &[String]) -> Option<Vec<Option<String>>> {
    let markers: Vec<(String, String)> = references
        .iter()
        .map(|_| {
            let id = uuid::Uuid::new_v4();
            (
                format!("--paperboy-op-begin-{id}--"),
                format!("--paperboy-op-end-{id}--"),
            )
        })
        .collect();

    let mut template = String::new();
    for (reference, (begin, end)) in references.iter().zip(&markers) {
        template.push_str(begin);
        template.push('\n');
        template.push_str("{{ ");
        template.push_str(reference);
        template.push_str(" }}\n");
        template.push_str(end);
        template.push('\n');
    }

    let output = run_cmd_with_stdin("op", &["inject"], &template)?;
    Some(
        markers
            .iter()
            .map(|(begin, end)| {
                let start = output.find(begin.as_str())? + begin.len();
                let stop = output[start..].find(end.as_str())?;
                // Trim exactly the single newlines the template inserted
                // around the placeholder, keeping any newlines that are
                // actually part of a multi-line secret value.
                Some(output[start..start + stop].trim_matches('\n').to_string())
            })
            .collect(),
    )
}

/// Run `program args…`, writing `input` to its stdin, and return raw stdout
/// (not trimmed, so multi-line output stays intact) or `None` if the program
/// is missing or exits non-zero.
fn run_cmd_with_stdin(program: &str, args: &[&str], input: &str) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run `program args…` and return trimmed stdout, or `None` if the program is
/// missing, exits non-zero, or produces empty output. `stdin` is closed so a
/// provider CLI never blocks waiting for interactive input.
fn run_cmd(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string();
    (!value.is_empty()).then_some(value)
}

/// Parse a `.vars` file, resolving provider references via the real CLIs.
/// Synchronous (blocks on the CLIs) — used by the headless CLI runner. The
/// GUIs use [`parse_vars_pending`] + [`spawn_resolution`] to avoid freezing.
pub fn parse_vars(name: String, content: &str) -> Environment {
    parse_vars_with(name, content, &default_resolver())
}

/// Resolve one value that may be a `{{ … }}` provider reference — an API key, a
/// token, anything held somewhere other than in the clear — to its literal.
///
/// A value with no reference in it is returned as-is (trimmed), so a caller can
/// pass whatever the user typed without deciding first what kind of thing it is.
/// `None` means the reference was recognised but the provider would not answer:
/// a wrong path, or a CLI that is missing or not signed in.
///
/// The reference goes through the same parser a `.vars` file does, so it
/// behaves identically here and there — the same syntax, the same providers,
/// and (like everything resolved this way) never written to disk.
///
/// **Blocking**: it shells out to `op`/`aws`, which can take seconds and may
/// prompt for a fingerprint. Never call it on a thread that is drawing.
pub fn resolve_reference(raw: &str) -> Option<String> {
    resolve_reference_with(raw, &default_resolver())
}

/// [`resolve_reference`] against a caller-supplied resolver, for tests.
pub fn resolve_reference_with(raw: &str, resolver: &dyn SecretResolver) -> Option<String> {
    let raw = raw.trim();
    if !raw.contains("{{") {
        return (!raw.is_empty()).then(|| raw.to_string());
    }
    let env = parse_vars_with("secret".to_string(), &format!("VALUE={raw}"), resolver);
    match env.vars.first() {
        Some(v) if v.resolved && !v.value.trim().is_empty() => Some(v.value.clone()),
        _ => None,
    }
}

/// Parse a `.vars` file using a caller-supplied `resolver`, resolving every
/// provider reference synchronously (used by the headless CLI and tests).
pub fn parse_vars_with(name: String, content: &str, resolver: &dyn SecretResolver) -> Environment {
    let (mut env, pending) = parse_vars_pending(name, content);
    resolve_pending_batched(&mut env, pending, resolver);
    env
}

/// Resolve a batch of [`PendingSecret`]s and apply the results to `env`.
/// Every `op://` reference is grouped and resolved together via
/// [`SecretResolver::resolve_op_batch`] so 1Password only needs to authorize
/// once for the whole batch, instead of once per reference; `ssm:` references
/// are resolved one at a time (the AWS CLI doesn't prompt interactively).
fn resolve_pending_batched(
    env: &mut Environment,
    pending: Vec<PendingSecret>,
    resolver: &dyn SecretResolver,
) {
    let mut op_refs = Vec::new();
    let mut op_indices = Vec::new();
    for p in &pending {
        if let SecretKind::Op(reference) = &p.kind {
            op_refs.push(reference.clone());
            op_indices.push(p.index);
        }
    }
    if !op_refs.is_empty() {
        let values = resolver.resolve_op_batch(&op_refs);
        for (index, value) in op_indices.into_iter().zip(values) {
            env.apply_update(&EnvUpdate {
                env_id: env.id,
                index,
                value,
            });
        }
    }
    for p in pending {
        if let SecretKind::Ssm(name) = &p.kind {
            let value = resolver.resolve_ssm(name);
            env.apply_update(&EnvUpdate {
                env_id: env.id,
                index: p.index,
                value,
            });
        }
    }
}

/// Parse a `.vars` file *without* blocking on external secret providers.
/// Literal and `env:` values resolve immediately; `op://` and `ssm:` references
/// are left marked `loading` and returned as [`PendingSecret`]s for the caller
/// to resolve in the background via [`spawn_resolution`].
pub fn parse_vars_pending(name: String, content: &str) -> (Environment, Vec<PendingSecret>) {
    let mut vars = Vec::new();
    let mut pending = Vec::new();

    // A Postman environment export (`.json`) is imported into the same model:
    // its variables are just `KEY`/value pairs, so they go through the identical
    // classification as `.vars` lines and behave the same from here on.
    let pairs: Vec<(String, String)> = match crate::postman::postman_env_values(content) {
        Some(pairs) => pairs,
        None => content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, raw)| (key.trim().to_string(), raw.trim().to_string()))
            .collect(),
    };

    for (key, raw) in pairs {
        let index = vars.len();
        vars.push(parse_var_pending(key, raw, index, &mut pending));
    }

    (
        Environment {
            id: next_env_id(),
            name,
            vars,
            path: None,
            git_origin: None,
        },
        pending,
    )
}

/// The result of classifying one variable's raw `.vars` token (e.g.
/// `{{ op://vault/item/field }}` or a plain literal string).
struct Classified {
    value: String,
    source: ValueSource,
    resolved: bool,
    loading: bool,
    /// `Some` when the token is a provider reference that still needs to be
    /// resolved in the background.
    secret: Option<SecretKind>,
}

/// Classify a raw `.vars` token: a plain string is an immediately-resolved
/// literal; `{{ env:NAME }}` is resolved immediately from the process
/// environment; `{{ ssm:… }}` / `{{ op://… }}` are left `loading` with a
/// [`SecretKind`] for the caller to resolve in the background.
fn classify_raw(raw: &str) -> Classified {
    let Some(inner) = extract_template(raw) else {
        return Classified {
            value: raw.to_string(),
            source: ValueSource::Literal,
            resolved: true,
            loading: false,
            secret: None,
        };
    };

    if let Some(env_name) = inner.strip_prefix("env:") {
        match std::env::var(env_name.trim()) {
            Ok(val) => Classified {
                value: val,
                source: ValueSource::ProcessEnv,
                resolved: true,
                loading: false,
                secret: None,
            },
            Err(_) => Classified {
                value: raw.to_string(),
                source: ValueSource::ProcessEnv,
                resolved: false,
                loading: false,
                secret: None,
            },
        }
    } else if let Some(name) = inner.strip_prefix("ssm:") {
        Classified {
            value: raw.to_string(),
            source: ValueSource::Ssm,
            resolved: false,
            loading: true,
            secret: Some(SecretKind::Ssm(name.trim().to_string())),
        }
    } else if inner.starts_with("op://") {
        Classified {
            value: raw.to_string(),
            source: ValueSource::OnePassword,
            resolved: false,
            loading: true,
            secret: Some(SecretKind::Op(inner.to_string())),
        }
    } else {
        Classified {
            value: raw.to_string(),
            source: ValueSource::Unknown,
            resolved: false,
            loading: false,
            secret: None,
        }
    }
}

/// Fast, non-blocking classification of one variable. Provider references are
/// recorded as pending and returned as an unresolved, `loading` [`EnvVar`].
fn parse_var_pending(
    key: String,
    raw: String,
    index: usize,
    pending: &mut Vec<PendingSecret>,
) -> EnvVar {
    let c = classify_raw(&raw);
    if let Some(kind) = c.secret {
        pending.push(PendingSecret { index, kind });
    }
    EnvVar {
        key,
        original_value: c.value.clone(),
        value: c.value,
        source: c.source,
        resolved: c.resolved,
        loading: c.loading,
        modified: false,
        user_added: false,
        raw,
    }
}

/// One environment's worth of pending secrets, tagged with its `env_id`, for
/// resolving several environments together via [`spawn_resolution_many`].
pub struct PendingEnvSecrets {
    pub env_id: u64,
    pub pending: Vec<PendingSecret>,
}

/// Like [`spawn_resolution`], but resolves the pending secrets of *multiple*
/// environments together: every `op://` reference across every group in
/// `groups` is gathered into a single [`SecretResolver::resolve_op_batch`]
/// call, so 1Password only prompts for authorization/unlock once even when
/// several collections (each with their own environment) are restored at the
/// same time — e.g. at app startup — instead of once per collection.
/// `ssm:` references still resolve individually (the AWS CLI doesn't prompt
/// interactively, so there's nothing to save by batching those).
pub fn spawn_resolution_many(groups: Vec<PendingEnvSecrets>) -> Receiver<EnvUpdate> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        #[cfg(not(test))]
        let resolver = CliResolver;
        #[cfg(test)]
        let resolver = NoopResolver;

        let mut op_refs = Vec::new();
        let mut op_targets = Vec::new();
        let mut ssm_targets = Vec::new();
        for group in groups {
            for p in group.pending {
                match p.kind {
                    SecretKind::Op(reference) => {
                        op_refs.push(reference);
                        op_targets.push((group.env_id, p.index));
                    }
                    SecretKind::Ssm(name) => ssm_targets.push((group.env_id, p.index, name)),
                }
            }
        }
        if !op_refs.is_empty() {
            let values = resolver.resolve_op_batch(&op_refs);
            for ((env_id, index), value) in op_targets.into_iter().zip(values) {
                if tx
                    .send(EnvUpdate {
                        env_id,
                        index,
                        value,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
        for (env_id, index, name) in ssm_targets {
            let value = resolver.resolve_ssm(&name);
            if tx
                .send(EnvUpdate {
                    env_id,
                    index,
                    value,
                })
                .is_err()
            {
                break;
            }
        }
    });
    rx
}

/// Resolve `pending` secrets on a background thread, streaming each result back
/// through the returned channel tagged with `env_id`.
pub fn spawn_resolution(env_id: u64, pending: Vec<PendingSecret>) -> Receiver<EnvUpdate> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // Production shells out to the `op`/`aws` CLIs; under test a no-op
        // resolver is used so the suite never depends on (or spawns) a real
        // provider CLI — results would otherwise vary by machine.
        #[cfg(not(test))]
        let resolver = CliResolver;
        #[cfg(test)]
        let resolver = NoopResolver;

        // Batch every `op://` reference into one `resolve_op_batch` call so
        // 1Password only prompts for authorization/unlock once per reload,
        // instead of once per secret variable.
        let mut op_refs = Vec::new();
        let mut op_indices = Vec::new();
        let mut rest = Vec::new();
        for p in pending {
            match &p.kind {
                SecretKind::Op(reference) => {
                    op_refs.push(reference.clone());
                    op_indices.push(p.index);
                }
                SecretKind::Ssm(_) => rest.push(p),
            }
        }
        if !op_refs.is_empty() {
            let values = resolver.resolve_op_batch(&op_refs);
            for (index, value) in op_indices.into_iter().zip(values) {
                // The receiver may have been dropped (e.g. environment
                // reloaded); stop early in that case.
                if tx
                    .send(EnvUpdate {
                        env_id,
                        index,
                        value,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
        for p in rest {
            let SecretKind::Ssm(name) = &p.kind else {
                unreachable!("rest only holds Ssm pending secrets")
            };
            let value = resolver.resolve_ssm(name);
            if tx
                .send(EnvUpdate {
                    env_id,
                    index: p.index,
                    value,
                })
                .is_err()
            {
                break;
            }
        }
    });
    rx
}

/// The resolver the synchronous entry points use.
///
/// Under test this is [`NoopResolver`], for the same reason the background
/// resolvers swap themselves out: a test that reached the real `op` would put a
/// fingerprint prompt on the developer's screen and resolve against whatever
/// happens to be in *their* 1Password — which is neither reproducible nor
/// something a test suite is entitled to do.
#[cfg(not(test))]
fn default_resolver() -> CliResolver {
    CliResolver
}

#[cfg(test)]
fn default_resolver() -> NoopResolver {
    NoopResolver
}

/// A resolver that never resolves anything, used under `cfg(test)` so the test
/// suite never invokes the real `op`/`aws` CLIs.
#[cfg(test)]
pub struct NoopResolver;

#[cfg(test)]
impl SecretResolver for NoopResolver {
    fn resolve_op(&self, _reference: &str) -> Option<String> {
        None
    }
    fn resolve_ssm(&self, _name: &str) -> Option<String> {
        None
    }
}

/// Extract the trimmed contents of a pure `{{ ... }}` expression.
fn extract_template(s: &str) -> Option<&str> {
    Some(s.trim().strip_prefix("{{")?.strip_suffix("}}")?.trim())
}

/// Heuristic check that `content` is an environment file: a Postman
/// environment export (`.json`), or a `.vars` file — at least one non-comment
/// `KEY=value` line whose key has no whitespace. This distinguishes it from a
/// Hurl collection (whose lines are `METHOD url` / `Header: value`).
pub fn looks_like_env(content: &str) -> bool {
    if crate::postman::postman_env_values(content).is_some() {
        return true;
    }
    content.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        match line.split_once('=') {
            Some((key, _)) => {
                let key = key.trim();
                !key.is_empty() && !key.contains(char::is_whitespace)
            }
            None => false,
        }
    })
}

/// The distinct `{{ KEY }}` variable names referenced in `text`, in first-seen
/// order (inner whitespace ignored).
pub fn referenced_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for caps in PLACEHOLDER.captures_iter(text) {
        let key = caps[1].to_string();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Replace each `{{ KEY }}` in `text` with its value from `vars`; unknown keys
/// are left verbatim. Single-pass, so an inserted value is never re-expanded.
pub fn substitute(text: &str, vars: &HashMap<String, String>) -> String {
    PLACEHOLDER
        .replace_all(text, |caps: &Captures| match vars.get(&caps[1]) {
            Some(value) => value.clone(),
            None => caps[0].to_string(),
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolver with two known references; everything else fails to resolve.
    struct MockResolver;
    impl SecretResolver for MockResolver {
        fn resolve_op(&self, reference: &str) -> Option<String> {
            (reference == "op://Engineering/demo-api/token").then(|| "op-secret-123".to_string())
        }
        fn resolve_ssm(&self, name: &str) -> Option<String> {
            (name == "/demo/api/password").then(|| "ssm-secret-xyz".to_string())
        }
    }

    /// A one-off secret — an API key typed into a wizard — resolves through the
    /// same syntax and the same providers a `.vars` file does.
    #[test]
    fn a_single_value_resolves_the_same_way_a_vars_line_does() {
        assert_eq!(
            resolve_reference_with("{{ op://Engineering/demo-api/token }}", &MockResolver),
            Some("op-secret-123".to_string())
        );
        assert_eq!(
            resolve_reference_with("{{ ssm:/demo/api/password }}", &MockResolver),
            Some("ssm-secret-xyz".to_string())
        );
    }

    /// Anything that isn't a reference is already the value: a caller can hand
    /// over whatever was typed without deciding first what kind of thing it is.
    #[test]
    fn a_plain_value_passes_straight_through() {
        assert_eq!(
            resolve_reference_with("  PMAK-abcdef  ", &MockResolver),
            Some("PMAK-abcdef".to_string())
        );
        assert_eq!(resolve_reference_with("   ", &MockResolver), None);
    }

    /// A reference the provider won't answer is not a key: reporting it as one
    /// would send the literal `{{ … }}` text to the server as a credential.
    #[test]
    fn an_unresolvable_reference_is_not_mistaken_for_a_value() {
        assert_eq!(
            resolve_reference_with("{{ op://Nope/nothing/here }}", &MockResolver),
            None
        );
    }

    fn one(content: &str) -> EnvVar {
        parse_vars_with("t".into(), content, &MockResolver)
            .vars
            .into_iter()
            .next()
            .unwrap()
    }

    /// A resolver that records every batch it's asked to resolve, so tests can
    /// assert that many `op://` references collapse into a single call
    /// (instead of one authorization prompt per secret).
    struct BatchCountingResolver {
        batch_calls: std::cell::RefCell<Vec<Vec<String>>>,
    }
    impl SecretResolver for BatchCountingResolver {
        fn resolve_op(&self, _reference: &str) -> Option<String> {
            panic!("resolve_op should not be called directly when resolve_op_batch is available");
        }
        fn resolve_ssm(&self, name: &str) -> Option<String> {
            (name == "/demo/api/password").then(|| "ssm-secret-xyz".to_string())
        }
        fn resolve_op_batch(&self, references: &[String]) -> Vec<Option<String>> {
            self.batch_calls.borrow_mut().push(references.to_vec());
            references
                .iter()
                .map(|r| Some(format!("resolved-{r}")))
                .collect()
        }
    }

    #[test]
    fn many_op_references_resolve_in_a_single_batch_call() {
        let resolver = BatchCountingResolver {
            batch_calls: std::cell::RefCell::new(Vec::new()),
        };
        let content = "A={{ op://v/i/a }}\nB=plain\nC={{ op://v/i/c }}\nD={{ ssm:/demo/api/password }}\nE={{ op://v/i/e }}";
        let env = parse_vars_with("t".into(), content, &resolver);

        let calls = resolver.batch_calls.borrow();
        assert_eq!(
            calls.len(),
            1,
            "all op:// references should resolve in exactly one batch call"
        );
        assert_eq!(calls[0], vec!["op://v/i/a", "op://v/i/c", "op://v/i/e"]);
        drop(calls);

        // Each resolved value must map back to the correct variable by index,
        // not just get shuffled into the batch's return order by coincidence.
        assert_eq!(env.vars[0].value, "resolved-op://v/i/a");
        assert_eq!(env.vars[1].value, "plain");
        assert_eq!(env.vars[2].value, "resolved-op://v/i/c");
        assert_eq!(
            env.vars[3].value, "ssm-secret-xyz",
            "ssm references still resolve alongside the op batch"
        );
        assert_eq!(env.vars[4].value, "resolved-op://v/i/e");
        assert!(env.vars.iter().all(|v| v.resolved));
    }

    #[test]
    fn resolves_op_reference() {
        let v = one("API_TOKEN={{ op://Engineering/demo-api/token }}");
        assert_eq!(v.value, "op-secret-123");
        assert!(v.resolved);
        assert_eq!(v.source, ValueSource::OnePassword);
    }

    #[test]
    fn resolves_ssm_reference() {
        let v = one("API_PASSWORD={{ ssm:/demo/api/password }}");
        assert_eq!(v.value, "ssm-secret-xyz");
        assert!(v.resolved);
        assert_eq!(v.source, ValueSource::Ssm);
    }

    #[test]
    fn unresolvable_op_falls_back_to_raw() {
        let v = one("X={{ op://Nope/missing/field }}");
        assert_eq!(v.value, "{{ op://Nope/missing/field }}");
        assert!(!v.resolved);
        assert_eq!(v.source, ValueSource::OnePassword);
    }

    #[test]
    fn unresolvable_ssm_falls_back_to_raw() {
        let v = one("X={{ ssm:/no/such/param }}");
        assert!(!v.resolved);
        assert_eq!(v.source, ValueSource::Ssm);
    }

    #[test]
    fn a_failed_op_reference_can_be_reloaded_once_the_provider_is_reachable() {
        let mut env = parse_vars_with("t".into(), "X={{ op://Nope/missing/field }}", &MockResolver);
        assert!(
            env.vars[0].is_failed(),
            "unresolved op:// reference should be flagged as failed"
        );

        // Retry with a resolver that now knows the reference (simulating the
        // provider becoming reachable / the user re-authorizing).
        struct NowWorksResolver;
        impl SecretResolver for NowWorksResolver {
            fn resolve_op(&self, reference: &str) -> Option<String> {
                (reference == "op://Nope/missing/field").then(|| "now-resolved".to_string())
            }
            fn resolve_ssm(&self, _name: &str) -> Option<String> {
                None
            }
        }
        let pending = env.vars[0].reload(0);
        assert!(
            env.vars[0].loading,
            "reload() should mark the variable loading again while it re-resolves"
        );
        let secret =
            pending.expect("a provider reference should return a PendingSecret to resolve");
        let value = NowWorksResolver
            .resolve_op_batch(&[match &secret.kind {
                SecretKind::Op(r) => r.clone(),
                SecretKind::Ssm(_) => unreachable!(),
            }])
            .remove(0);
        env.apply_update(&EnvUpdate {
            env_id: env.id,
            index: secret.index,
            value,
        });
        assert!(env.vars[0].resolved);
        assert!(!env.vars[0].is_failed());
        assert_eq!(env.vars[0].value, "now-resolved");
    }

    #[test]
    fn reload_is_a_no_op_for_a_variable_that_did_not_fail() {
        let mut env = parse_vars_with("t".into(), "A=plain-value", &MockResolver);
        assert!(!env.vars[0].is_failed());
        assert!(
            env.vars[0].reload(0).is_none(),
            "reload() should do nothing for an already-resolved literal"
        );
        assert_eq!(env.vars[0].value, "plain-value");
    }

    #[test]
    fn reload_re_reads_a_process_env_reference_that_previously_failed() {
        // Ensure the variable is absent before the first parse.
        // SAFETY: single-threaded test, no other thread reads/writes this var.
        unsafe { std::env::remove_var("PAPERBOY_TEST_RELOAD_VAR") };
        let mut env = parse_vars_with(
            "t".into(),
            "X={{ env:PAPERBOY_TEST_RELOAD_VAR }}",
            &MockResolver,
        );
        assert!(
            env.vars[0].is_failed(),
            "a missing process env var should be flagged as failed"
        );

        // SAFETY: single-threaded test, no other thread reads/writes this var.
        unsafe { std::env::set_var("PAPERBOY_TEST_RELOAD_VAR", "now-set") };
        let pending = env.vars[0].reload(0);
        assert!(
            pending.is_none(),
            "env: references resolve synchronously, with no PendingSecret to hand off"
        );
        assert!(env.vars[0].resolved);
        assert_eq!(env.vars[0].value, "now-set");
        // SAFETY: single-threaded test, no other thread reads/writes this var.
        unsafe { std::env::remove_var("PAPERBOY_TEST_RELOAD_VAR") };
    }

    #[test]
    fn literal_values_are_untouched() {
        let v = one("A=plain-value");
        assert_eq!(v.value, "plain-value");
        assert!(v.resolved);
        assert_eq!(v.source, ValueSource::Literal);
    }

    #[test]
    fn resolved_secrets_are_masked_for_display() {
        let op = one("API_TOKEN={{ op://Engineering/demo-api/token }}");
        assert!(op.is_secret());
        assert_eq!(op.display_value(), SECRET_MASK);
        assert_ne!(
            op.value, SECRET_MASK,
            "the real value is still available to send"
        );

        let ssm = one("API_PASSWORD={{ ssm:/demo/api/password }}");
        assert!(ssm.is_secret());
        assert_eq!(ssm.display_value(), SECRET_MASK);
    }

    #[test]
    fn non_secrets_display_their_real_value() {
        // literal
        assert!(!one("A=plain").is_secret());
        assert_eq!(one("A=plain").display_value(), "plain");
        // unresolved provider ref is not a secret — show the reference to aid debugging
        let unresolved = one("X={{ op://Nope/missing/field }}");
        assert!(!unresolved.is_secret());
        assert_eq!(unresolved.display_value(), "{{ op://Nope/missing/field }}");
    }

    #[test]
    fn env_reference_reads_process_environment() {
        // SAFETY: single-threaded test using a unique variable name.
        unsafe { std::env::set_var("PAPERBOY_TEST_ENV_VAR", "from-env") };
        let v = one("T={{ env:PAPERBOY_TEST_ENV_VAR }}");
        assert_eq!(v.value, "from-env");
        assert!(v.resolved);
        assert_eq!(v.source, ValueSource::ProcessEnv);
        unsafe { std::env::remove_var("PAPERBOY_TEST_ENV_VAR") };
    }

    #[test]
    fn pending_parse_leaves_providers_loading() {
        let content = "A=plain\nB={{ op://Vault/item/field }}\nC={{ ssm:/demo/p }}";
        let (env, pending) = parse_vars_pending("t".into(), content);

        assert_eq!(env.vars.len(), 3);
        // Literal resolves instantly.
        assert!(env.vars[0].resolved && !env.vars[0].loading);
        // Both providers are left loading and unresolved (no blocking CLI call).
        assert!(env.vars[1].loading && !env.vars[1].resolved);
        assert_eq!(env.vars[1].source, ValueSource::OnePassword);
        assert!(env.vars[2].loading && !env.vars[2].resolved);
        assert_eq!(env.vars[2].source, ValueSource::Ssm);
        // Both providers are returned as pending work.
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn apply_update_resolves_success_and_ignores_other_envs() {
        let (mut env, pending) = parse_vars_pending("t".into(), "B={{ op://V/i/f }}");
        let idx = pending[0].index;

        env.apply_update(&EnvUpdate {
            env_id: env.id,
            index: idx,
            value: Some("secret".into()),
        });
        assert!(env.vars[idx].resolved && !env.vars[idx].loading);
        assert_eq!(env.vars[idx].value, "secret");
        assert!(env.vars[idx].is_secret());

        // An update for a different environment id is ignored.
        env.apply_update(&EnvUpdate {
            env_id: env.id + 99,
            index: idx,
            value: Some("x".into()),
        });
        assert_eq!(env.vars[idx].value, "secret");
    }

    #[test]
    fn user_edits_flag_modified_relative_to_the_original() {
        let mut v = one("NAME=plain");
        assert!(!v.modified, "freshly loaded vars are unmodified");
        assert_eq!(v.original_value, "plain");

        v.set_user_value_secrecy("changed".into(), true, 0);
        assert!(v.modified, "a different value marks the var modified");
        assert_eq!(v.value, "changed");
        assert_eq!(
            v.original_value, "plain",
            "the original is preserved for reset"
        );

        v.set_user_value_secrecy("plain".into(), true, 0);
        assert!(
            !v.modified,
            "restoring the original value clears the modified flag"
        );
    }

    #[test]
    fn resolution_sets_the_reset_target_and_keeps_user_edits() {
        let (mut env, pending) = parse_vars_pending("t".into(), "B={{ op://V/i/f }}");
        let idx = pending[0].index;

        // The user edits the value while the secret is still loading.
        env.vars[idx].set_user_value_secrecy("user-typed".into(), true, idx);
        assert!(env.vars[idx].modified);

        // Resolution completes: it must not clobber the user's edit, but should
        // record the resolved value as the reset target.
        env.apply_update(&EnvUpdate {
            env_id: env.id,
            index: idx,
            value: Some("real-secret".into()),
        });
        assert_eq!(env.vars[idx].value, "user-typed", "user edit is preserved");
        assert_eq!(
            env.vars[idx].original_value, "real-secret",
            "reset restores the resolved value"
        );
        assert!(env.vars[idx].resolved);
    }

    #[test]
    fn apply_update_failure_stays_pending() {
        let (mut env, _p) = parse_vars_pending("t".into(), "B={{ ssm:/demo/p }}");
        env.apply_update(&EnvUpdate {
            env_id: env.id,
            index: 0,
            value: None,
        });
        assert!(!env.vars[0].resolved);
        assert!(!env.vars[0].loading);
        // A failed secret is still "pending" so requests stay blocked.
        assert!(env.vars[0].is_pending());
    }

    #[test]
    fn referenced_keys_finds_unique_placeholders() {
        let keys = referenced_keys("{{ BASE_URL }}/x/{{TOKEN}} plain {{ BASE_URL }}");
        assert_eq!(keys, vec!["BASE_URL".to_string(), "TOKEN".to_string()]);
        assert!(referenced_keys("no placeholders here").is_empty());
    }

    #[test]
    fn substitute_replaces_known_keys_regardless_of_inner_spacing() {
        let vars = HashMap::from([("TOKEN".to_string(), "secret".to_string())]);
        // Tight, single-space and multi-space spellings must all resolve to the
        // same variable (multi-space was silently left unsubstituted before).
        assert_eq!(substitute("{{TOKEN}}", &vars), "secret");
        assert_eq!(substitute("{{ TOKEN }}", &vars), "secret");
        assert_eq!(substitute("a {{   TOKEN   }} b", &vars), "a secret b");
    }

    #[test]
    fn substitute_keeps_unknown_placeholders_verbatim() {
        let vars = HashMap::new();
        assert_eq!(substitute("{{ MISSING }}", &vars), "{{ MISSING }}");
    }

    #[test]
    fn substitute_does_not_re_expand_inserted_values() {
        // A value that itself looks like a placeholder must be inserted as-is,
        // never re-scanned for further substitution.
        let vars = HashMap::from([("A".to_string(), "{{ B }}".to_string())]);
        assert_eq!(substitute("{{ A }}", &vars), "{{ B }}");
    }

    #[test]
    fn looks_like_env_accepts_vars_files_and_rejects_collections() {
        assert!(looks_like_env(
            "# comment\nTOKEN={{ op://a/b/c }}\nBASE=http://x"
        ));
        assert!(looks_like_env("A=1"));

        // A Hurl collection is not an environment file.
        let collection =
            "# Health\nGET http://127.0.0.1:8080/health\nAuthorization: Bearer x\nHTTP 200";
        assert!(!looks_like_env(collection));
        // Blank / comment-only content is not an environment file.
        assert!(!looks_like_env("\n\n# just a comment\n"));
        // A URL query string ("KEY=val" but the key has spaces) is not a var line.
        assert!(!looks_like_env("GET http://x?a=b"));
    }

    /// A Postman environment export is an environment file too, in both the
    /// bare shape and the `{"environment": …}` envelope an account backup uses.
    #[test]
    fn postman_environment_exports_are_loaded_as_environments() {
        let bare = r#"{
          "id": "abc", "name": "Staging",
          "values": [
            { "key": "url", "value": "https://staging.example", "enabled": true },
            { "key": "token", "value": "t0k", "type": "secret" },
            { "key": "old", "value": "gone", "enabled": false }
          ]
        }"#;
        let enveloped = format!("{{ \"environment\": {} }}", bare);

        for content in [bare, enveloped.as_str()] {
            assert!(looks_like_env(content));
            let (env, pending) = parse_vars_pending("Staging".into(), content);
            assert!(pending.is_empty(), "plain literals need no resolution");
            assert_eq!(
                env.vars.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(),
                vec!["url", "token"],
                "a variable Postman disabled is not imported"
            );
            assert_eq!(env.vars[0].value, "https://staging.example");
            assert!(env.vars[0].resolved);
            // `raw` is what a later "Save Environment" writes out as `.vars`.
            assert_eq!(env.to_vars_text(), "url=https://staging.example\ntoken=t0k");
        }
    }

    /// A Postman value written as a provider reference is classified like the
    /// same token in a `.vars` file, rather than being taken literally.
    #[test]
    fn postman_environment_value_can_be_a_provider_reference() {
        let content = r#"{ "name": "e", "values": [
            { "key": "TOKEN", "value": "{{ op://V/i/f }}" }
        ]}"#;
        let (env, pending) = parse_vars_pending("e".into(), content);
        assert_eq!(env.vars[0].source, ValueSource::OnePassword);
        assert_eq!(pending.len(), 1);
    }

    /// A Postman *collection* must not be mistaken for an environment.
    #[test]
    fn postman_collection_is_not_an_environment() {
        let json = r#"{ "collection": {
          "info": { "name": "demo" },
          "item": [ { "name": "a", "request": { "method": "GET", "url": "http://x" } } ]
        }}"#;
        assert!(!looks_like_env(json));
    }

    /// Regression: a `.vars` line cannot carry a newline, and the reader drops
    /// any line without an `=`, so a pasted multi-line secret used to come back
    /// as its first line alone — losing the rest on save and on every restart.
    /// Flattening at entry keeps what is stored and what is shown the same.
    #[test]
    fn a_multi_line_value_survives_a_save_and_reload_intact() {
        let pem = "-----BEGIN PRIVATE KEY-----\nAAAA\nBBBB\n-----END PRIVATE KEY-----";
        let mut env = parse_vars("dev".into(), "");
        env.vars.push(EnvVar::user("TLS_KEY".into(), pem.into()));
        let stored = env.vars[0].value.clone();
        assert!(
            !stored.contains('\n'),
            "the value is flattened as it is taken"
        );

        let reloaded = parse_vars("dev".into(), &env.to_vars_text());
        assert_eq!(
            reloaded.vars[0].value, stored,
            "and what comes back is what was shown, not a truncation of it"
        );
    }
}
