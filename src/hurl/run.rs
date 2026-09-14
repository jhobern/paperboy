//! Execution + evaluation boundary for the Hurl runner.
//!
//! Internally the app keeps its own editable [`HurlEntry`](super::HurlEntry)
//! model; here we serialize it to Hurl text and hand it to the `hurl` crate's
//! runner, which performs the HTTP request(s) and evaluates every `[Captures]`
//! and `[Asserts]` (the full Hurl feature set). The result is mapped back into
//! the app's response/capture/assert model.
//!
//! All runner stdout/stderr is captured in buffered terminals so nothing leaks
//! to the real terminal (which would corrupt the TUI); any error text is
//! returned via [`RunOutput::error`] for display on the status bar instead.

use std::collections::HashMap;
use std::path::Path;

use hurl::runner::{
    self, AssertResult, EntryResult, EventListener, RunnerError, RunnerOptionsBuilder, Value,
    VariableSet,
};
use hurl::util::logger::{Logger, LoggerOptionsBuilder};
use hurl::util::path::ContextDir;
use hurl::util::term::{Stderr, Stdout, WriteMode};
use hurl_core::error::DisplaySourceError;
use hurl_core::parser::parse_hurl_file;
use serde_json::Value as JsonValue;

/// Outcome of evaluating one explicit `[Asserts]` expression, for the response
/// panel. The `hurl` runner does the actual evaluation.
#[derive(Debug, Clone)]
pub struct AssertOutcome {
    /// The assert expression text (e.g. `jsonpath "$.status" == "ok"`).
    pub expr: String,
    pub passed: bool,
    /// A short reason shown on failure; empty on success.
    pub detail: String,
}

/// The mapped result of running one Hurl entry.
///
/// `Default` exists for tests, which mostly care about two or three fields and
/// would otherwise have to spell out a dozen empty ones to say so.
#[derive(Default)]
pub struct EntryOutcome {
    /// Which request in the collection this is the result of (0-based).
    ///
    /// Not the same as the outcome's position in the result list: a request
    /// carrying `[Options] repeat: N` — or one that fails and is retried —
    /// produces several outcomes, all bearing the index of the single request
    /// that produced them. Counting outcomes instead would slide every later
    /// result up by one and show a response against the wrong request, which
    /// in an API client is the worst kind of wrong.
    pub entry_index: usize,
    /// A retry attempt that a later attempt of the same request replaced.
    ///
    /// `[Options] retry` means "keep asking until it holds", so a poll that
    /// answered `ResultUnavailable` twice and then succeeded is one request
    /// that passed -- not two failures and a pass. Hurl returns every attempt
    /// and settles the question with the last one for a given entry (its own
    /// `is_success` does exactly this), so PaperBoy marks the superseded ones
    /// here rather than leaving each reader of the result list to rediscover
    /// the rule and disagree about it.
    ///
    /// Only ever set for a request that asks to be retried. `[Options] repeat`
    /// also produces several outcomes per request, but those are N real runs
    /// the user asked for, and every one of them counts.
    pub superseded: bool,
    /// The method/URL actually sent (fully substituted, incl. chained captures).
    pub method: String,
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// Response body, pretty-printed when it parses as JSON.
    pub body: String,
    /// The response body exactly as received (never reformatted), so a report
    /// can render `RESPONSE RAW` without losing the server's original bytes
    /// (whitespace, key order, non-JSON payloads). `body` is the pretty view of
    /// this same content.
    pub raw_body: String,
    pub asserts: Vec<AssertOutcome>,
    pub captures: Vec<(String, String)>,
    /// Effective duration of the HTTP transfer(s) for this entry, in
    /// milliseconds (excludes assert/capture processing). Reports surface this
    /// as the per-request "Time" column.
    pub duration_ms: u64,
    /// The `duration_ms` breakdown, from libcurl's own timers, so a report can
    /// separate what the *server* did from what the local machine and network
    /// spent getting to it. Each is summed over the entry's calls (a redirect
    /// chain contributes every hop), so the three always add up to
    /// `duration_ms`.
    ///
    /// Connection setup: DNS resolution, TCP connect and the TLS handshake —
    /// everything before the request could start being sent. PaperBoy builds a
    /// fresh client per request, so every request pays this in full; under a
    /// heavily parallel run it is also the part that suffers most from local
    /// CPU and uplink contention, which is precisely why it is worth seeing
    /// apart from the rest.
    pub setup_ms: u64,
    /// Time in flight: from the request starting to go out to the first byte of
    /// the response arriving. The closest thing to "what the server took",
    /// and the figure to watch when a parallel run makes `Time` climb.
    pub wait_ms: u64,
    /// Time spent receiving the response body, after its first byte.
    pub download_ms: u64,
    /// `true` when the runner reported no errors for this entry (status
    /// expectation, asserts and transport all satisfied).
    pub ok: bool,
    /// The first runner error for this entry (transport / failed assert / status
    /// mismatch), rendered concisely; `None` when the entry passed.
    pub error: Option<String>,
}

/// What [`run_hurl_streaming_with`]'s `before_entry` hook decided about the
/// entry it was called for.
pub enum EntrySetup {
    /// Bind these values over the run's variables and send the request.
    Bind(Vec<(String, String)>),
    /// Don't send it. `reason` is reported against the entry, which is marked
    /// failed.
    ///
    /// This exists for the `# [Gen]` block: a row that fails to evaluate
    /// leaves its name unbound, and *something else* of that name -- an
    /// environment value, a capture from an earlier request -- is then used in
    /// its place. The request goes out looking perfectly well-formed, signed
    /// with the wrong key, and is answered. A request whose block failed must
    /// not be sent at all, exactly as a single send refuses it.
    Skip { reason: String },
}

/// The mapped result of a whole run (one or more entries).
pub struct RunOutput {
    pub entries: Vec<EntryOutcome>,
    /// A concise message for the status bar: a parse error, transport failure,
    /// or the first failed assertion. `None` when everything succeeded.
    pub error: Option<String>,
    /// What the request's `# [Gen]` block computed for *this* send.
    ///
    /// A generated value is an output of the send just as a capture is — the
    /// difference is only that it was computed before the request left rather
    /// than read out of the response. It has to travel with the result for the
    /// same reason a capture does: something downstream may name it, and the
    /// alternative is that `{{step.sid}}` resolves to whatever older value of
    /// that name happens to be lying around. Empty for every runner that does
    /// not evaluate a `[Gen]` block, which is all of them but the live one.
    pub generated: std::collections::HashMap<String, String>,
}

/// Builds the [`ContextDir`] that gates local file access for `[Form]`/
/// `[Multipart]` file fields (and `[Options] output`), matching the real
/// `hurl` CLI's own default: `file_root` is the directory containing the
/// `.hurl`/collection file (so a relative form file path like `avatar.png`
/// resolves next to it, exactly as the user expects), falling back to the
/// process's current directory when no source file is known (e.g. an
/// unsaved/remote collection). `current_dir` is always the process's actual
/// working directory, so absolute paths (as produced by the file picker)
/// resolve correctly too.
fn context_dir(file_root: Option<&Path>) -> ContextDir {
    let current_dir = std::env::current_dir().unwrap_or_default();
    let file_root = file_root.unwrap_or(&current_dir);
    ContextDir::new(&current_dir, file_root)
}

/// Parse and run `content` (Hurl text) with `vars` as substitution variables,
/// mapping the runner's result into the app model. Captures from earlier entries
/// flow into later ones automatically within this call. `file_root` should be
/// the collection's source directory, if known, so relative `[Form]`/
/// `[Multipart]` file paths resolve against it (see [`context_dir`]).
pub fn run_hurl(
    content: &str,
    vars: &HashMap<String, String>,
    file_root: Option<&Path>,
) -> RunOutput {
    run_hurl_watching(content, vars, file_root, |_, _, _| {})
}

/// [`run_hurl`], plus a hook called as each attempt at an entry starts.
///
/// `on_attempt` receives `(entry index, attempt number counting from 0, the
/// request's [`RetryLimit`])`. Everything a retry does happens
/// inside the single `run_entries` call below, so this is the only way for a
/// caller to say anything at all while a poll is in progress — see
/// [`AttemptReporter`].
pub fn run_hurl_watching(
    content: &str,
    vars: &HashMap<String, String>,
    file_root: Option<&Path>,
    mut on_attempt: impl FnMut(usize, usize, RetryLimit),
) -> RunOutput {
    let hurl_file = match parse_hurl_file(content) {
        Ok(h) => h,
        Err(e) => {
            return RunOutput {
                entries: vec![],
                error: Some(format!("Parse error (line {}): {:?}", e.pos.line, e.kind)),
                generated: Default::default(),
            };
        }
    };

    let runner_opts = RunnerOptionsBuilder::new()
        .continue_on_error(true)
        .context_dir(&context_dir(file_root))
        .build();
    let logger_opts = LoggerOptionsBuilder::new().build();
    let mut variables = VariableSet::new();
    for (k, v) in vars {
        variables.insert(k.clone(), Value::String(v.clone()));
    }
    let secrets = variables.secrets();
    // Buffered terminals: the runner's output/errors are captured, not written
    // to the real terminal.
    let mut stdout = Stdout::new(WriteMode::Buffered);
    let mut logger = Logger::new(&logger_opts, Stderr::new(WriteMode::Buffered), &secrets);

    // One limit for the whole file: a `run_hurl` call is a single request in
    // every front-end that reports attempts, so there is no per-entry ambiguity
    // worth carrying here.
    let reporter = AttemptReporter {
        on_attempt: std::cell::RefCell::new(&mut on_attempt),
        limit: hurl_file
            .entries
            .first()
            .map(|e| entry_retry_limit(e, &variables))
            .unwrap_or_default(),
    };
    let result = runner::run_entries(
        &hurl_file.entries,
        content,
        None,
        &runner_opts,
        &variables,
        &mut stdout,
        Some(&reporter),
        &mut logger,
    );

    let lines: Vec<&str> = content.lines().collect();
    let mut entries = Vec::new();
    let mut errors: Vec<Option<String>> = Vec::new();

    for e in &result.entries {
        let (outcome, entry_error) = map_entry_result(e, &lines);
        entries.push(outcome);
        errors.push(entry_error);
    }
    mark_superseded(&mut entries, |i| {
        hurl_file.entries.get(i).is_some_and(entry_retries)
    });
    // The run's error is the first *surviving* failure. Taking the first of any
    // kind would report a poll that eventually succeeded as a failed run, on
    // the strength of the attempt that was supposed to be thrown away.
    let error = entries
        .iter()
        .zip(&errors)
        .find_map(|(e, err)| (!e.superseded).then_some(err.clone()).flatten());

    RunOutput {
        entries,
        error,
        generated: Default::default(),
    }
}

/// How many attempts a request allows -- the denominator in "retry 2 of 5",
/// when there is one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RetryLimit {
    /// Nothing honest to count towards: no `retry:` row at all, or one written
    /// `{{name}}` against a name nothing binds (a run Hurl is about to stop
    /// anyway) or holding something that is not a number. A hint says which
    /// attempt is running and stops there rather than inventing a total.
    #[default]
    Unknown,
    /// `retry: -1` -- keep asking until it holds.
    Forever,
    /// `retry: N`.
    Times(usize),
}

impl RetryLimit {
    /// The total to put after "retry 2 of", when there is one worth putting
    /// there. `∞` rather than nothing for a forever-poll: it keeps the shape of
    /// the sentence and says the useful thing, which is that this request will
    /// go on asking. It is spelled the same in every language PaperBoy speaks,
    /// which is why it is here and not in the string table.
    pub fn total(self) -> Option<String> {
        match self {
            RetryLimit::Times(n) => Some(n.to_string()),
            RetryLimit::Forever => Some("∞".to_string()),
            RetryLimit::Unknown => None,
        }
    }
}

impl From<i64> for RetryLimit {
    /// Hurl's own reading of the number: negative is "forever" (it is written
    /// `-1`), anything else is a count.
    fn from(n: i64) -> Self {
        usize::try_from(n).map_or(RetryLimit::Forever, RetryLimit::Times)
    }
}

/// Read the retry limit off a request, resolving a `{{placeholder}}` against
/// the variables the request is about to run with.
///
/// Matched on the AST rather than on the option's printed form, because `Count`
/// prints "forever" as `-1`: reading the text would quietly turn the one case
/// worth naming into "no limit stated".
///
/// A placeholder is looked *up* rather than given up on -- `retry:
/// {{max_attempts}}` is an ordinary way to write a poll, and the value is right
/// here in the same variable set Hurl is about to substitute from. In practice
/// that name has to have been *captured*: Hurl demands a number there, and
/// every variable PaperBoy binds from an environment file is text, so an
/// environment-set limit is a run Hurl stops with "Invalid expression type"
/// before any of this matters. It does not stand in a number of its own in that
/// case -- no default, no "don't retry" -- so `Unknown` is only ever reported
/// for a request that is not about to retry at all. Reading it
/// early cannot go stale, either: Hurl resolves an entry's options once, before
/// its retry loop starts (`get_entry_options` is called outside `run_request`),
/// so a capture made *during* the poll does not change the limit the poll is
/// running under -- which is exactly the number reported here.
fn entry_retry_limit(entry: &hurl_core::ast::Entry, vars: &VariableSet) -> RetryLimit {
    use hurl_core::ast::{CountOption, ExprKind, OptionKind};
    use hurl_core::types::Count;
    entry
        .request
        .options()
        .iter()
        .find_map(|opt| match &opt.kind {
            OptionKind::Retry(CountOption::Literal(Count::Finite(n))) => {
                Some(RetryLimit::Times(*n))
            }
            OptionKind::Retry(CountOption::Literal(Count::Infinite)) => Some(RetryLimit::Forever),
            OptionKind::Retry(CountOption::Placeholder(p)) => Some(match &p.expr.kind {
                ExprKind::Variable(v) => vars
                    .get(&v.name)
                    .and_then(|v| v.value().to_string().trim().parse::<i64>().ok())
                    .map_or(RetryLimit::Unknown, RetryLimit::from),
                // A generator function (`{{newUuid}}` and friends) is never a
                // count.
                ExprKind::Function(_) => RetryLimit::Unknown,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

/// Hurl's own progress hook, forwarded to a PaperBoy closure.
///
/// This is the only way to know about a retry *while it is happening*: a
/// retried entry's results all arrive together when the runner finally gives
/// up or succeeds, so without this a request configured `retry: 5,
/// retry-interval: 10000` sits silently for the best part of a minute. Hurl
/// fires the event just *before* it sleeps for the interval, precisely so a
/// front-end can say what the wait is for.
struct AttemptReporter<'a> {
    /// `RefCell` because the trait hands out `&self`, while a caller that wants
    /// to record what it sees needs `&mut`.
    on_attempt: std::cell::RefCell<&'a mut dyn FnMut(usize, usize, RetryLimit)>,
    limit: RetryLimit,
}

impl EventListener for AttemptReporter<'_> {
    fn on_entry_running(
        &self,
        current: hurl_core::types::Index,
        _last: hurl_core::types::Index,
        retry_count: usize,
    ) {
        (self.on_attempt.borrow_mut())(current.to_zero_based(), retry_count, self.limit);
    }
}

/// Whether a request asks Hurl to retry it until its asserts pass.
///
/// Read from the parsed entry rather than from PaperBoy's own `[Options]` rows,
/// because this has to agree with what the runner actually did -- and the
/// runner read the same AST.
fn entry_retries(entry: &hurl_core::ast::Entry) -> bool {
    use hurl_core::ast::OptionKind;
    entry
        .request
        .options()
        .iter()
        .any(|opt| matches!(opt.kind, OptionKind::Retry(_)))
}

/// Mark every outcome that a later attempt of the same request replaced.
///
/// `outcomes` must be the results of one runner call, in the order the runner
/// produced them: attempts of one entry are consecutive, so "another result
/// with my index follows" is exactly "I was retried".
fn mark_superseded(outcomes: &mut [EntryOutcome], retries: impl Fn(usize) -> bool) {
    for i in 0..outcomes.len().saturating_sub(1) {
        let index = outcomes[i].entry_index;
        if outcomes[i + 1].entry_index == index && retries(index) {
            outcomes[i].superseded = true;
        }
    }
}

/// The `variable:` `[Options]` a parsed Hurl entry declares, as `(name, value)`
/// pairs — the defaults a `# [Gen]` block is allowed to read (see the streaming
/// runner above). Placeholder-valued definitions are rendered as written; a
/// literal like `SAMPLE_KEY=s3cret` comes back verbatim.
fn entry_variable_defaults(entry: &hurl_core::ast::Entry) -> Vec<(String, String)> {
    use hurl_core::ast::OptionKind;
    entry
        .request
        .options()
        .iter()
        .filter_map(|opt| match &opt.kind {
            OptionKind::Variable(def) => Some((def.name.clone(), def.value.to_string())),
            _ => None,
        })
        .collect()
}

/// Like [`run_hurl`], but invokes `on_entry` immediately after each request
/// finishes, instead of only returning once the whole collection has run — so a
/// caller can stream results out as they happen — and gives `before_entry` a
/// chance to bind extra variables just before each entry runs.
///
/// Each entry runs via its own [`runner::run_entries`] call, windowed to just
/// that one entry (`from_entry`/`to_entry`); `[Captures]` still flow from one
/// entry to the next exactly as in a single full run, by threading the same
/// `VariableSet` returned by each call into the next one. The one behavioural
/// difference from `run_hurl`: Hurl's automatic cookie jar (cookies
/// remembered from `Set-Cookie` response headers) does *not* carry across
/// entries in this mode, since each call starts a fresh HTTP client — an
/// explicit `[Cookies]` section on a request is unaffected either way.
///
/// `before_entry` is called with the entry's zero-based index and everything
/// currently bound — the environment, plus whatever earlier entries captured —
/// and whatever it returns is bound over the top for that entry onwards. This
/// is how a `# [Gen]` block reaches a whole-collection run: the block belongs to
/// one request and is evaluated per send, so it cannot be folded into the run's
/// variables up front, and evaluating it here is also what lets a generator
/// read a value an earlier request captured.
pub fn run_hurl_streaming_with(
    content: &str,
    vars: &HashMap<String, String>,
    file_root: Option<&Path>,
    mut before_entry: impl FnMut(usize, &HashMap<String, String>) -> EntrySetup,
    mut on_entry: impl FnMut(&EntryOutcome),
    mut on_attempt: impl FnMut(usize, usize, RetryLimit),
) -> RunOutput {
    let hurl_file = match parse_hurl_file(content) {
        Ok(h) => h,
        Err(e) => {
            return RunOutput {
                entries: vec![],
                error: Some(format!("Parse error (line {}): {:?}", e.pos.line, e.kind)),
                generated: Default::default(),
            };
        }
    };

    let ctx_dir = context_dir(file_root);
    let logger_opts = LoggerOptionsBuilder::new().build();
    let mut variables = VariableSet::new();
    for (k, v) in vars {
        variables.insert(k.clone(), Value::String(v.clone()));
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut error: Option<String> = None;
    let mut entries = Vec::new();
    let total = hurl_file.entries.len();

    for i in 1..=total {
        let mut known: HashMap<String, String> = variables
            .iter()
            .map(|(k, v)| (k.clone(), v.value().to_string()))
            .collect();
        // Layer this entry's own `[Options] variable:` rows in as defaults
        // before the block is evaluated, so a generator can read a parameter
        // the request declares — `sig = hmac_sha256(SAMPLE_KEY, "m")` with
        // `[Options] variable: SAMPLE_KEY=…`. A single send does exactly this
        // (`effective_vars_reporting` folds the defaults in first); without it
        // the block was evaluated before Hurl applies the option, failed on the
        // undefined name, and the request went out with a literal `{{sig}}`.
        // Layered into a *copy* only, not into `variables`: the surviving option
        // rows are still applied by Hurl during the run, and binding them here
        // would leak a per-entry default into later entries.
        for (name, value) in entry_variable_defaults(&hurl_file.entries[i - 1]) {
            known.entry(name).or_insert(value);
        }
        match before_entry(i - 1, &known) {
            EntrySetup::Bind(bindings) => {
                for (k, v) in bindings {
                    variables.insert(k, Value::String(v));
                }
            }
            EntrySetup::Skip { reason } => {
                // Reported as a finished, failed entry rather than as a gap:
                // the caller's `on_entry` is what stamps the pass/fail marker
                // and fills the per-entry response, so an entry that is simply
                // not mentioned stays "still running" for the rest of the run.
                // Method and URL as written (unsubstituted -- nothing was
                // resolved for this entry) so the line printed for it still
                // says which request it is.
                let req = &hurl_file.entries[i - 1].request;
                let outcome = EntryOutcome {
                    entry_index: i - 1,
                    method: req.method.to_string(),
                    url: req.url.to_string(),
                    ok: false,
                    error: Some(reason.clone()),
                    ..Default::default()
                };
                if error.is_none() {
                    error = Some(reason);
                }
                on_entry(&outcome);
                entries.push(outcome);
                continue;
            }
        }
        let runner_opts = RunnerOptionsBuilder::new()
            .continue_on_error(true)
            .from_entry(Some(i))
            .to_entry(Some(i))
            .context_dir(&ctx_dir)
            .build();
        let secrets = variables.secrets();
        let mut stdout = Stdout::new(WriteMode::Buffered);
        let mut logger = Logger::new(&logger_opts, Stderr::new(WriteMode::Buffered), &secrets);

        // Fresh per entry: the reporter borrows the caller's closure, and the
        // borrow only has to last as long as this one window's run.
        let reporter = AttemptReporter {
            on_attempt: std::cell::RefCell::new(&mut on_attempt),
            limit: entry_retry_limit(&hurl_file.entries[i - 1], &variables),
        };
        let result = runner::run_entries(
            &hurl_file.entries,
            content,
            None,
            &runner_opts,
            &variables,
            &mut stdout,
            Some(&reporter),
            &mut logger,
        );
        // Carry captures forward into the next entry's window.
        variables = result.variables;

        // Mapped as a batch before any is reported: whether an attempt was
        // superseded is only knowable once the attempt after it is in hand, and
        // `on_entry` is what stamps the caller's pass/fail marker.
        let mut window: Vec<EntryOutcome> = Vec::new();
        let mut window_errors: Vec<Option<String>> = Vec::new();
        for e in &result.entries {
            let (outcome, entry_error) = map_entry_result(e, &lines);
            window.push(outcome);
            window_errors.push(entry_error);
        }
        let retried = entry_retries(&hurl_file.entries[i - 1]);
        mark_superseded(&mut window, |_| retried);
        for (outcome, entry_error) in window.into_iter().zip(window_errors) {
            if error.is_none() && !outcome.superseded {
                error = entry_error;
            }
            on_entry(&outcome);
            entries.push(outcome);
        }
    }

    RunOutput {
        entries,
        error,
        generated: Default::default(),
    }
}

/// Map one runner [`EntryResult`] to the app's [`EntryOutcome`], returning it
/// alongside its own concise error (if any) for the caller to fold into the
/// whole run's status. Shared by [`run_hurl`] and [`run_hurl_streaming_with`] so
/// both stay in lockstep on exactly what gets surfaced from a Hurl result.
fn map_entry_result(e: &EntryResult, lines: &[&str]) -> (EntryOutcome, Option<String>) {
    let (method, url) = e
        .calls
        .last()
        .map(|c| (c.request.method.clone(), c.request.url.to_string()))
        .unwrap_or_default();
    let (status, headers, body, raw_body) = match e.calls.last() {
        Some(call) => {
            let r = &call.response;
            let hdrs = r
                .headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect();
            // Decompress by `Content-Encoding` first: when a request sends its
            // own `Accept-Encoding` header, libcurl won't auto-decode, so
            // `r.body` is still the compressed bytes. `uncompress_body` honours
            // the header (and no-ops when absent); fall back to the raw bytes if
            // the stream is malformed.
            let bytes = r.uncompress_body().unwrap_or_else(|_| r.body.clone());
            let raw = String::from_utf8_lossy(&bytes).to_string();
            let body = serde_json::from_str::<JsonValue>(&raw)
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.clone()))
                .unwrap_or_else(|_| raw.clone());
            (r.status as u16, hdrs, body, raw)
        }
        None => (0, Vec::new(), String::new(), String::new()),
    };

    // Only surface EXPLICIT [Asserts] plus the implicit status assertion; the
    // implicit HTTP-version assert stays folded into the status/version line.
    let mut asserts = Vec::new();
    // Surface the implicit HTTP status assertion (the `HTTP <code>` response
    // line) as a leading `status == <code>` row, so the response's [Asserts]
    // view shows the status check alongside the explicit asserts — Hurl treats
    // the status line as an assertion too. `HTTP *` / no status line produces
    // no `ImplicitStatus`, so nothing is shown in that case.
    for a in &e.asserts {
        if let AssertResult::ImplicitStatus {
            actual, expected, ..
        } = a
        {
            let failed = a.to_runner_error().is_some();
            asserts.push(AssertOutcome {
                expr: format!("status == {expected}"),
                passed: !failed,
                detail: if failed {
                    format!("got {actual}")
                } else {
                    String::new()
                },
            });
        }
    }
    for a in &e.asserts {
        if !matches!(a, AssertResult::Explicit { .. }) {
            continue;
        }
        let err = a.to_runner_error();
        let expr = lines
            .get(a.line().saturating_sub(1))
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        let detail = err.as_ref().map(assert_detail).unwrap_or_default();
        asserts.push(AssertOutcome {
            expr,
            passed: err.is_none(),
            detail,
        });
    }

    let captures = e
        .captures
        .iter()
        .map(|c| (c.name.clone(), c.value.to_string()))
        .collect();

    // A failed status assertion gets a clear "expected X but got Y" message
    // (with the request that produced it) rather than the runner's terse
    // "Assert status code: HTTP 200", which hides both the expected and the
    // actual status. Other errors (transport, failed explicit asserts) keep
    // their concise per-line rendering.
    let status_mismatch = e.asserts.iter().find_map(|a| match a {
        AssertResult::ImplicitStatus {
            actual, expected, ..
        } if a.to_runner_error().is_some() => Some((*actual, *expected)),
        _ => None,
    });
    let entry_error = if let Some((actual, expected)) = status_mismatch {
        let reason = reason(actual as u16);
        let actual_txt = if reason.is_empty() {
            format!("{actual}")
        } else {
            format!("{actual} {reason}")
        };
        Some(format!(
            "Expected status {expected} but got {actual_txt} ({method} {url})"
        ))
    } else {
        // The first error (transport failure or failed assert) is surfaced
        // per-entry and, for the whole run, on the status bar.
        e.errors.first().map(|er| render_error(er, lines))
    };

    // Split the transfer time into connection setup / in-flight / download,
    // summed across calls so a redirect chain accounts for every hop. libcurl
    // reports its timers as offsets from the start of each transfer:
    // `pre_transfer` is "connected, TLS done, about to send", `start_transfer`
    // is "first response byte in". Saturating throughout: the timers are
    // independent samples and a stalled transfer can report them out of order,
    // which must never underflow into an absurd duration.
    let (setup_ms, wait_ms, download_ms) = e.calls.iter().fold((0, 0, 0), |(s, w, d), c| {
        let t = &c.timings;
        let pre = t.pre_transfer;
        let start = t.start_transfer.max(pre);
        let total = t.total.max(start);
        (
            s + pre.as_millis() as u64,
            w + (start - pre).as_millis() as u64,
            d + (total - start).as_millis() as u64,
        )
    });

    (
        EntryOutcome {
            entry_index: e.entry_index.to_zero_based(),
            // Decided by `mark_superseded` once the attempt after this one is
            // known; a single result is never superseded.
            superseded: false,
            method,
            url,
            status,
            status_text: reason(status).to_string(),
            headers,
            body,
            raw_body,
            asserts,
            captures,
            duration_ms: e.transfer_duration.as_millis() as u64,
            setup_ms,
            wait_ms,
            download_ms,
            ok: e.errors.is_empty(),
            error: entry_error.clone(),
        },
        entry_error,
    )
}

/// A short "actual vs expected"-style detail for a failed assert.
fn assert_detail(e: &RunnerError) -> String {
    e.description()
}

/// A concise, single-line rendering of a runner error for the status bar:
/// its description plus the offending source line when available.
fn render_error(e: &RunnerError, lines: &[&str]) -> String {
    let desc = e.description();
    let line = e.source_info().start.line;
    match lines.get(line.saturating_sub(1)) {
        Some(l) if !l.trim().is_empty() => format!("{desc}: {}", l.trim()),
        _ => desc,
    }
}

/// Canonical reason phrase for common status codes (the runner exposes only the
/// numeric status).
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes a path when it goes out of scope, so a failing assertion can't
    /// leave scratch files behind. The plain `remove_*` call these tests used to
    /// end with is skipped when the test panics — which for the one case that
    /// has to write into the *current* directory meant litter in the working
    /// tree, not just in `/tmp`.
    struct TempPath(std::path::PathBuf);

    impl Drop for TempPath {
        fn drop(&mut self) {
            if self.0.is_dir() {
                std::fs::remove_dir_all(&self.0).ok();
            } else {
                std::fs::remove_file(&self.0).ok();
            }
        }
    }

    impl std::ops::Deref for TempPath {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl TempPath {
        /// The path itself. Inherent rather than left to `Deref`, because
        /// `Path::as_path` doesn't exist and the call would otherwise resolve
        /// through a different (unstable) trait entirely.
        fn as_path(&self) -> &std::path::Path {
            &self.0
        }
    }

    /// A uniquely-named scratch path under `dir` (not created on disk), cleaned
    /// up whether the test passes or panics.
    fn temp_path(dir: &std::path::Path, prefix: &str) -> TempPath {
        TempPath(dir.join(format!("{prefix}_{}", uuid::Uuid::new_v4())))
    }

    /// Spawn a one-shot HTTP/1.1 server on an ephemeral port that answers the
    /// first connection with `status`/`reason` and a tiny JSON body, then
    /// closes. Returns the bound port. Used to exercise the status-assertion
    /// mapping against a real (local) response without any network access.
    fn one_shot_server(status: u16, reason: &str) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reason = reason.to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let body = "{\"ok\":true}";
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        port
    }

    /// A server that answers "not ready" `pending` times and then succeeds,
    /// on an ephemeral port. The shape a polling request is written against.
    fn polling_server(pending: usize) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut seen = 0;
            while let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                seen += 1;
                let state = if seen > pending { "Matched" } else { "Pending" };
                // `attempts` is there for the tests that need a *number* to
                // capture: Hurl types a captured JSON number as one, which is
                // the only way a `retry: {{max_attempts}}` can work (see
                // `entry_retry_limit`).
                let body = format!("{{\"result\":\"{state}\",\"attempts\":4}}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        port
    }

    /// A poll that succeeds on its third go is one request that passed, not two
    /// failures and a pass. Hurl hands back every attempt and settles it with
    /// the last (its own `is_success` does the same); PaperBoy used to treat
    /// each attempt as an outcome in its own right, so the request it had just
    /// retried into success was reported as a failure -- `[Options] retry`
    /// undone at the last step, and exactly what a converted Postman polling
    /// loop relies on.
    #[test]
    fn a_poll_that_succeeds_on_the_third_go_is_a_pass() {
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: 5\nretry-interval: 20\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let out = run_hurl(&content, &HashMap::new(), None);

        assert_eq!(out.entries.len(), 3, "every attempt is still reported");
        let surviving: Vec<&EntryOutcome> = out.entries.iter().filter(|e| !e.superseded).collect();
        assert_eq!(surviving.len(), 1, "one request, one outcome that counts");
        assert!(surviving[0].ok, "{:?}", surviving[0].error);
        assert!(
            out.error.is_none(),
            "the run reported {:?} for a poll that succeeded",
            out.error
        );
    }

    /// Every attempt is announced *as it starts*, which is the only way a
    /// front-end can say anything at all during a poll: all of a retried
    /// entry's results arrive together when it finally settles, so a request
    /// written `retry: 30, retry-interval: 2000` would otherwise be a minute of
    /// silence indistinguishable from a hung connection.
    #[test]
    fn each_attempt_is_reported_while_the_poll_is_still_running() {
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: 5\nretry-interval: 20\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut seen: Vec<(usize, usize, RetryLimit)> = Vec::new();
        let out = run_hurl_watching(&content, &HashMap::new(), None, |i, attempt, limit| {
            seen.push((i, attempt, limit))
        });

        assert!(out.entries.iter().any(|e| e.ok), "the poll did succeed");
        assert_eq!(
            seen,
            vec![
                (0, 0, RetryLimit::Times(5)),
                (0, 1, RetryLimit::Times(5)),
                (0, 2, RetryLimit::Times(5))
            ],
            "the first send plus the two retries, each with the stated limit"
        );
    }

    /// A request nobody retries reports its one attempt and no limit, so a
    /// caller can tell "sending" from "retrying" by the attempt number alone.
    #[test]
    fn a_request_that_is_not_retried_reports_a_single_first_attempt() {
        let port = polling_server(0);
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let mut seen: Vec<(usize, usize, RetryLimit)> = Vec::new();
        let _ = run_hurl_watching(&content, &HashMap::new(), None, |i, attempt, limit| {
            seen.push((i, attempt, limit))
        });

        assert_eq!(seen, vec![(0, 0, RetryLimit::Unknown)]);
    }

    /// `retry: {{max_attempts}}` is an ordinary way to write a poll, and the
    /// value is in the same variable set Hurl is about to substitute from, so
    /// the limit is looked up rather than given up on. (It cannot go stale
    /// either: Hurl resolves an entry's options once, before its retry loop
    /// starts, so a capture made during the poll does not change the limit the
    /// poll is running under.)
    ///
    /// Captured, not passed in: Hurl demands a *number* there, and every
    /// variable PaperBoy binds from an environment file is text.
    #[test]
    fn a_placeholder_limit_is_resolved_from_the_variables() {
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/\nHTTP 200\n[Captures]\n\
             max_attempts: jsonpath \"$.attempts\"\n\n\
             GET http://127.0.0.1:{port}/\n[Options]\nretry: {{{{max_attempts}}}}\n\
             retry-interval: 20\nHTTP 200\n[Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut limits: Vec<(usize, RetryLimit)> = Vec::new();
        let out = run_hurl_streaming_with(
            &content,
            &HashMap::new(),
            None,
            |_, _| EntrySetup::Bind(Vec::new()),
            |_| {},
            |i, _, limit| limits.push((i, limit)),
        );

        assert!(
            out.entries.iter().any(|e| e.entry_index == 1 && e.ok),
            "the poll did succeed: {:?}",
            out.error
        );
        assert!(
            limits
                .iter()
                .filter(|(i, _)| *i == 1)
                .all(|(_, l)| *l == RetryLimit::Times(4)),
            "the placeholder should have been looked up, got {limits:?}"
        );
    }

    /// A name nothing binds has no total to report -- and inventing one would
    /// be worse than saying nothing, since the only thing a count is good for
    /// is being trusted. (Hurl refuses the run outright in this case; the hint
    /// still has to say something sensible for the one attempt that is made.)
    #[test]
    fn a_placeholder_limit_that_resolves_to_nothing_has_no_total() {
        let port = polling_server(1);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: {{{{max_attempts}}}}\n\
             retry-interval: 20\nHTTP 200\n[Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut limits: Vec<RetryLimit> = Vec::new();
        let _ = run_hurl_watching(&content, &HashMap::new(), None, |_, _, limit| {
            limits.push(limit)
        });

        assert!(
            limits.iter().all(|l| *l == RetryLimit::Unknown),
            "got {limits:?}"
        );
    }

    /// A limit Hurl cannot make a number of is not quietly taken as some
    /// particular number -- it fails the entry, before the request is sent, and
    /// explicitly without retrying (an error evaluating an entry's options is
    /// not a retryable one). Which is what makes `RetryLimit::Unknown` safe to
    /// report: there is never a retry for it to be wrong about.
    ///
    /// Pinned because the alternative -- a silent fallback to "don't retry", or
    /// to some default count -- would turn a typo in a poll's limit into a
    /// request that quietly checks once and reports whatever it happened to
    /// find, which is the failure mode `retry` exists to remove.
    #[test]
    fn a_limit_hurl_cannot_resolve_stops_the_entry_rather_than_standing_in_for_a_number() {
        let port = polling_server(99);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: {{{{n}}}}\nretry-interval: 20\n\
             HTTP 200\n[Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        for (label, vars, expected) in [
            ("undefined", HashMap::new(), "Undefined variable"),
            // Every value PaperBoy binds from an environment file is text, even
            // one that reads like a number -- so this is the case a user who
            // writes `retry: {{max_attempts}}` in a `.vars` file actually hits.
            (
                "text that looks like a number",
                HashMap::from([("n".to_string(), "3".to_string())]),
                "Invalid expression type",
            ),
        ] {
            let mut retries = 0;
            let out = run_hurl_watching(&content, &vars, None, |_, attempt, _| {
                retries = retries.max(attempt)
            });

            assert_eq!(retries, 0, "{label}: nothing should have been retried");
            assert_eq!(out.entries.len(), 1, "{label}: the request was not sent");
            let error = out.error.unwrap_or_default();
            assert!(
                error.contains(expected),
                "{label}: expected {expected:?}, got {error:?}"
            );
        }
    }

    /// `retry: -1` is "forever": it has no number to count towards, but that is
    /// worth saying rather than leaving out -- the reader's question is "will
    /// this stop?", and the answer is no.
    #[test]
    fn a_forever_retry_is_reported_as_forever_not_as_no_limit() {
        let port = polling_server(1);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: -1\nretry-interval: 20\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut limits: Vec<RetryLimit> = Vec::new();
        let _ = run_hurl_watching(&content, &HashMap::new(), None, |_, _, limit| {
            limits.push(limit)
        });

        assert!(
            limits.iter().all(|l| *l == RetryLimit::Forever),
            "retry: -1 is forever, not an unknown limit; got {limits:?}"
        );
    }

    /// Streaming reports attempts per entry, and the index is the entry's --
    /// not the attempt's ordinal, which is what a retry makes different.
    #[test]
    fn streaming_reports_which_entry_is_being_retried() {
        // Two pending answers: one spent on `/first`, so `/second` needs a
        // retry to see "Matched".
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/first\nHTTP 200\n\n\
             GET http://127.0.0.1:{port}/second\n[Options]\nretry: 3\nretry-interval: 20\n\
             HTTP 200\n[Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut seen: Vec<(usize, usize, RetryLimit)> = Vec::new();
        let _ = run_hurl_streaming_with(
            &content,
            &HashMap::new(),
            None,
            |_, _| EntrySetup::Bind(Vec::new()),
            |_| {},
            |i, attempt, limit| seen.push((i, attempt, limit)),
        );

        assert_eq!(
            seen,
            vec![
                (0, 0, RetryLimit::Unknown),
                (1, 0, RetryLimit::Times(3)),
                (1, 1, RetryLimit::Times(3))
            ],
            "the retry belongs to the second entry, and only it has a limit"
        );
    }

    /// The other half of the rule: retries that never succeed are still a
    /// failure, and the *last* attempt is the one that says so.
    #[test]
    fn a_poll_that_never_comes_good_still_fails() {
        let port = polling_server(99);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: 1\nretry-interval: 20\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let out = run_hurl(&content, &HashMap::new(), None);
        let surviving: Vec<&EntryOutcome> = out.entries.iter().filter(|e| !e.superseded).collect();
        assert_eq!(surviving.len(), 1);
        assert!(!surviving[0].ok);
        assert!(out.error.is_some(), "a failed run must say why");
    }

    /// `repeat` is not `retry`: those are N runs the user asked for, and every
    /// one of them counts. Nothing may be marked away, or a repeat that failed
    /// twice and passed once would report as a pass.
    #[test]
    fn a_repeated_request_keeps_every_run() {
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nrepeat: 3\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let out = run_hurl(&content, &HashMap::new(), None);
        assert_eq!(out.entries.len(), 3);
        assert!(
            out.entries.iter().all(|e| !e.superseded),
            "a repeat's runs are all real"
        );
        assert!(out.error.is_some(), "two of the three runs failed");
    }

    /// The streaming path runs each entry in its own window, so it has to apply
    /// the same rule on its own -- and it is the path the terminal UI and a
    /// plain `paperboy -c` both take.
    #[test]
    fn streaming_marks_the_superseded_attempts_too() {
        let port = polling_server(2);
        let content = format!(
            "GET http://127.0.0.1:{port}/\n[Options]\nretry: 5\nretry-interval: 20\nHTTP 200\n\
             [Asserts]\njsonpath \"$.result\" == \"Matched\"\n"
        );
        let mut seen: Vec<(usize, bool, bool)> = Vec::new();
        let out = run_hurl_streaming_with(
            &content,
            &HashMap::new(),
            None,
            |_, _| EntrySetup::Bind(Vec::new()),
            |eo| seen.push((eo.entry_index, eo.ok, eo.superseded)),
            |_, _, _| {},
        );
        assert_eq!(
            seen,
            vec![(0, false, true), (0, false, true), (0, true, false)],
            "the caller is told which attempts to ignore, as they happen"
        );
        assert!(out.error.is_none(), "{:?}", out.error);
    }

    /// Feature: the transfer time is reported both whole and broken into its
    /// connection-setup / in-flight / download parts, and the parts add up.
    #[test]
    fn timing_breakdown_partitions_the_total() {
        let port = one_shot_server(200, "OK");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        // Millisecond truncation of each part can lose up to 1ms apiece, so the
        // sum can trail the total slightly; it must never exceed it.
        let parts = e.setup_ms + e.wait_ms + e.download_ms;
        assert!(
            parts <= e.duration_ms && e.duration_ms - parts <= 3,
            "parts {parts} should account for total {}",
            e.duration_ms
        );
    }

    /// Feature: the implicit `HTTP <code>` status line surfaces in the mapped
    /// asserts as a `status == <code>` row (so the response's [Asserts] view
    /// shows the status check), and passes when the status matches.
    #[test]
    fn status_line_appears_as_a_passing_assert() {
        let port = one_shot_server(200, "OK");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(e.ok, "entry should pass, error: {:?}", e.error);
        let status_assert = e
            .asserts
            .iter()
            .find(|a| a.expr == "status == 200")
            .expect("a `status == 200` assert row");
        assert!(status_assert.passed);
    }

    /// Spawn a one-shot server that answers with a gzip-compressed body and a
    /// `Content-Encoding: gzip` header (but no `Content-Length`, closing the
    /// connection to signal end-of-body). Mirrors a server honouring a request's
    /// own `Accept-Encoding` header — the case libcurl leaves un-decoded.
    fn one_shot_gzip_server(gzip_body: &'static [u8]) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head);
                let _ = sock.write_all(gzip_body);
                let _ = sock.flush();
            }
        });
        port
    }

    /// Feature: a gzip response is decompressed for display. When a server
    /// returns `Content-Encoding: gzip` (as it does for a request that sends its
    /// own `Accept-Encoding`), libcurl doesn't auto-decode, so the mapping must
    /// uncompress the body itself — otherwise the raw compressed bytes would be
    /// shown (the garbled-output bug).
    #[test]
    fn gzip_response_body_is_decompressed() {
        // gzip of `{"ok":true}` (mtime=0 for a stable literal).
        static GZIP_OK: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xab, 0x56, 0xca, 0xcf,
            0x56, 0xb2, 0x2a, 0x29, 0x2a, 0x4d, 0xad, 0x05, 0x00, 0x90, 0x5f, 0xd4, 0xa7, 0x0b,
            0x00, 0x00, 0x00,
        ];
        let port = one_shot_gzip_server(GZIP_OK);
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(e.ok, "entry should pass, error: {:?}", e.error);
        // raw_body is the exact decompressed bytes; body is its pretty JSON view.
        assert_eq!(e.raw_body, "{\"ok\":true}");
        assert!(
            e.body.contains("\"ok\": true"),
            "body should be decompressed pretty JSON, got: {:?}",
            e.body
        );
    }

    /// Feature: a failed status assertion is both surfaced as a failed
    /// `status == <expected>` assert row (with the actual status in its
    /// detail) and rendered as a clear "expected X but got Y" error message
    /// naming the request — not the runner's terse "Assert status code".
    #[test]
    fn failed_status_assertion_has_a_clear_message() {
        let port = one_shot_server(404, "Not Found");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(!e.ok);
        let status_assert = e
            .asserts
            .iter()
            .find(|a| a.expr == "status == 200")
            .expect("a `status == 200` assert row");
        assert!(!status_assert.passed);
        assert!(
            status_assert.detail.contains("404"),
            "detail should show the actual status, got: {}",
            status_assert.detail
        );
        let msg = e.error.as_deref().unwrap_or_default();
        assert!(
            msg.contains("Expected status 200") && msg.contains("got 404"),
            "message should state expected vs actual, got: {msg}"
        );
        assert!(
            !msg.contains("Assert status code"),
            "message should not be the terse runner default, got: {msg}"
        );
    }

    /// A `HTTP *` wildcard status line asserts nothing about the status, so no
    /// synthetic `status == …` row is produced.
    #[test]
    fn wildcard_status_line_produces_no_status_assert() {
        let port = one_shot_server(200, "OK");
        let content = format!("GET http://127.0.0.1:{port}/\nHTTP *\n");
        let out = run_hurl(&content, &HashMap::new(), None);
        let e = out.entries.first().expect("one entry");
        assert!(
            !e.asserts.iter().any(|a| a.expr.starts_with("status ==")),
            "HTTP * should not synthesize a status assert"
        );
    }

    /// A `[Multipart]` file field referenced by a path relative to the
    /// collection's own directory must be authorized when `file_root` is
    /// passed through — this is the regression this test guards: before
    /// `context_dir(...)` was wired into the runner options, every relative
    /// (and most absolute) form-file path was rejected with "Unauthorized
    /// file access", regardless of where the `.hurl` file actually lived.
    #[test]
    fn relative_form_file_path_is_authorized_against_the_collection_directory() {
        let dir = temp_path(&std::env::temp_dir(), "paperboy_run_test");
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(dir.join("avatar.png"), b"fake-png").unwrap();

        // The URL is unroutable (TEST-NET-1, RFC 5737) with a tiny implicit
        // timeout via an unreachable address; either way, the file-access
        // check happens before any network I/O, so the result is
        // deterministic regardless of network availability.
        let content = "POST http://192.0.2.1/upload\n[Multipart]\navatar: file,avatar.png;\n";
        let out = run_hurl(content, &HashMap::new(), Some(dir.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "a form file relative to the collection directory must be authorized, got: {msg}"
        );
    }

    /// Without a `file_root`, the runner falls back to the process's current
    /// directory (matching the real `hurl` CLI's own behaviour when no
    /// `.hurl` source path or explicit `--file-root` is known).
    #[test]
    fn missing_file_root_falls_back_to_the_process_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        let file_path = temp_path(&cwd, "paperboy_run_test_cwd");
        let unique = file_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::fs::write(&*file_path, b"fake").unwrap();

        let content = format!("POST http://192.0.2.1/upload\n[Multipart]\nf: file,{unique};\n");
        let out = run_hurl(&content, &HashMap::new(), None);

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "a file in the process's current directory must be authorized when no file_root is given, got: {msg}"
        );
    }

    /// A form file path that resolves outside the given `file_root` must
    /// still be rejected — the fix must not disable the sandbox entirely.
    #[test]
    fn form_file_path_outside_the_file_root_is_still_rejected() {
        let root = temp_path(&std::env::temp_dir(), "paperboy_run_test_root");
        let outside = temp_path(&std::env::temp_dir(), "paperboy_run_test_outside");
        std::fs::create_dir_all(&*root).unwrap();
        std::fs::create_dir_all(&*outside).unwrap();
        std::fs::write(outside.join("secret.bin"), b"fake").unwrap();

        let content = "POST http://192.0.2.1/upload\n[Multipart]\nf: file,../secret.bin;\n";
        // "../secret.bin" relative to `root` escapes it entirely (it doesn't
        // even land inside `outside`), so this must be rejected.
        let out = run_hurl(content, &HashMap::new(), Some(root.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            msg.to_ascii_lowercase().contains("unauthorized"),
            "a file outside file_root must still be rejected, got: {msg}"
        );
    }

    /// End-to-end proof that staging fixes the exact scenario the previous
    /// test guards against: an out-of-scope form file, once staged via
    /// `stage_out_of_scope_form_files`, is authorized by `run_hurl` even
    /// though it was rejected before staging.
    #[test]
    fn staging_authorizes_a_form_file_that_would_otherwise_be_rejected() {
        use crate::hurl::entry::{FormField, FormFieldKind, HurlEntry};
        use crate::hurl::stage_out_of_scope_form_files;

        let root = temp_path(&std::env::temp_dir(), "paperboy_stage_run_root");
        let outside = temp_path(&std::env::temp_dir(), "paperboy_stage_run_outside");
        std::fs::create_dir_all(&*root).unwrap();
        std::fs::create_dir_all(&*outside).unwrap();
        let outside_file = outside.join("secret.bin");
        std::fs::write(&outside_file, b"fake").unwrap();

        let mut entries = vec![HurlEntry {
            method: "POST".into(),
            url: "http://192.0.2.1/upload".into(),
            form_fields: vec![FormField {
                key: "f".into(),
                value: outside_file.to_string_lossy().into_owned(),
                kind: FormFieldKind::File,
                ..Default::default()
            }],
            ..Default::default()
        }];

        let staged = stage_out_of_scope_form_files(&mut entries, Some(root.as_path())).unwrap();
        assert!(
            staged.is_some(),
            "an out-of-scope file must trigger staging"
        );
        let staged_dir = staged.unwrap();

        let content = entries[0].to_hurl();
        let out = run_hurl(&content, &HashMap::new(), Some(staged_dir.as_path()));

        let msg = out
            .entries
            .first()
            .and_then(|e| e.error.as_deref())
            .unwrap_or_default();
        assert!(
            !msg.to_ascii_lowercase().contains("unauthorized"),
            "the staged file must be authorized against the staging directory, got: {msg}"
        );

        std::fs::remove_dir_all(&staged_dir).ok();
    }

    /// An error-status response must keep its body. A 4xx/5xx payload is
    /// usually the *most* interesting thing on screen — it carries the API's
    /// explanation of what went wrong — so losing it would be worse than
    /// losing a 200's. Checked with and without an `HTTP <code>` expectation
    /// line, because a failed status assertion takes a different path through
    /// `map_entry_result` (it fills in `error`) and must not discard the
    /// response it is complaining about.
    #[test]
    fn error_status_responses_keep_their_body() {
        for status in [400u16, 401, 404, 422, 500, 502, 503] {
            // No expectation line: the entry passes, and the body is the point.
            let port = one_shot_server(status, "Err");
            let content = format!("GET http://127.0.0.1:{port}/\n");
            let out = run_hurl(&content, &HashMap::new(), None);
            let e = out.entries.first().expect("one entry");
            assert_eq!(e.status, status);
            assert!(e.ok, "no expectation means nothing to fail: {:?}", e.error);
            assert_eq!(
                e.raw_body, "{\"ok\":true}",
                "the {status} body must survive verbatim"
            );
            assert!(
                e.body.contains("\"ok\""),
                "and be pretty-printed for display: {:?}",
                e.body
            );

            // Now with an expectation the response fails: the entry is marked
            // failed and carries an error, but the body must still be there.
            let port = one_shot_server(status, "Err");
            let content = format!("GET http://127.0.0.1:{port}/\nHTTP 200\n");
            let out = run_hurl(&content, &HashMap::new(), None);
            let e = out.entries.first().expect("one entry");
            assert_eq!(e.status, status);
            assert!(!e.ok, "expected 200, got {status}");
            assert!(
                e.error.as_deref().unwrap_or_default().contains("200"),
                "the mismatch is reported: {:?}",
                e.error
            );
            assert_eq!(
                e.raw_body, "{\"ok\":true}",
                "a failed status assert must not discard the {status} body"
            );
        }
    }

    /// Answer `n` connections in turn, echoing the requested path back in the
    /// body so the responses of several requests can be told apart.
    fn echo_path_server(n: usize) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut sock, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let read = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..read]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = format!("{{\"path\":\"{path}\"}}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        port
    }

    /// Regression: `[Options] repeat` makes one request produce several
    /// outcomes. Each must name the request it came from, because that index
    /// is what the runner keys results by — counting outcomes instead slid
    /// every later result up and showed one request's response against the
    /// next one.
    #[test]
    fn every_outcome_of_a_repeated_request_names_that_request() {
        let port = echo_path_server(3);
        let content = format!(
            "GET http://127.0.0.1:{port}/first\n[Options]\nrepeat: 2\nHTTP 200\n\nGET http://127.0.0.1:{port}/second\nHTTP 200\n"
        );
        let out = run_hurl(&content, &HashMap::new(), None);
        assert_eq!(out.entries.len(), 3, "two repeats, then the second request");
        assert_eq!(
            out.entries
                .iter()
                .map(|e| e.entry_index)
                .collect::<Vec<_>>(),
            vec![0, 0, 1],
            "both repeats belong to request 0"
        );
        assert!(
            out.entries[2].url.ends_with("/second"),
            "and the last outcome really is the second request"
        );
    }
}
