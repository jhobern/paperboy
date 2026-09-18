//! Computed request values — the `# [Gen]` block's expression language.
//!
//! Postman lets a pre-request script build values a request needs but can't
//! state literally: a nonce, a timestamp, an HMAC signature over the two.
//! PaperBoy has no JavaScript engine and doesn't want one, so it offers a fixed
//! set of functions instead, evaluated just before the request is sent.
//!
//! # Why the expression isn't in the placeholder
//!
//! The obvious design — `{{ hmac_sha256(KEY, MSG) }}` — cannot work. Hurl reads
//! a variable name only as far as the first character outside `A-Z a-z 0-9 _ -`
//! and then discards the remainder *without an error*, so that placeholder is
//! sent as the value of `hmac_sha256` (see
//! [`placeholder_problem`](crate::hurl::placeholder_problem), which now refuses
//! it outright). Expressions therefore live in a `# [Gen]` comment block and the
//! request refers to their results as ordinary `{{name}}` variables.
//!
//! The happy consequence is portability: the `.hurl` file stays valid, stock
//! `hurl` parses it identically, and running it elsewhere needs only
//! `--variable name=…`. A missing value is a loud `Undefined variable` rather
//! than a wrong request.
//!
//! # Grammar
//!
//! ```text
//! expr   := call | ident | string | number
//! call   := ident '(' [ expr { ',' expr } ] ')'
//! ident  := name character run — a variable reference, or a function name
//! string := '"' … '"' with \n \t \r \" \\ escapes
//! ```
//!
//! A bare identifier is a *variable reference*, resolved from the same map the
//! request is substituted with — so a generator can read an environment
//! variable, a request parameter, or an earlier generator. Calls nest, because
//! signing is built by nesting: `base64(hmac_sha256(K, concat(A, B)))`.
//!
//! # Signing
//!
//! `md5`/`sha1`/`sha256`/`sha512` hash, `hmac_sha1`/`hmac_sha256`/`hmac_sha512`
//! sign, each also as a `_b64` variant. The encoding is in the name because a
//! signature in the wrong one is the right length, entirely plausible to look
//! at, and rejected with the same 401 as a wrong secret — a default would be a
//! thing to get wrong silently. Bare is hex, matching `sha256sum` and
//! CryptoJS's `toString()`, which is what a script being ported from Postman
//! expects; `_b64` is standard padded Base64, which is what most APIs that
//! don't want hex want.
//!
//! Note that `base64(sha256(m))` is *not* `sha256_b64(m)`: the former encodes
//! the 64 characters of hex, the latter the 32 bytes they spell. That is what
//! the `_b64` variants are for.
//!
//! What this cannot do is chain a MAC into the *key* of the next one, because
//! every value here is text and a digest's bytes only survive as hex. AWS
//! SigV4's four-step key derivation therefore isn't expressible. Nothing is
//! lost in practice yet: SigV4 also needs a canonical request built from the
//! live request's headers, which is a different feature entirely.
//!
//! Arguments cannot be `{{ … }}` placeholders. PaperBoy's own substitution is
//! single-pass and its pattern can't nest, so a nested placeholder would never
//! be expanded; a bare name means the same thing and always works.

use std::collections::HashMap;

/// The outside world a generator is allowed to touch: the clock, the random
/// number source, and the run's counters.
///
/// Injected rather than called directly so tests are deterministic — the same
/// reason `Importer` holds its clock (see `postman_import.rs`). A signature is
/// only checkable against a known-good vector if the nonce and timestamp that
/// went into it can be pinned.
/// What the request a `[Gen]` block belongs to is about to send, for the
/// functions that read it (`method`, `url`, `path`, `query`, `header`, `body`,
/// `request_name`).
///
/// Held as written -- `{{ name }}` and all -- and substituted at the moment a
/// function asks for it, against the variables known *at that point in the
/// block*. That is the only ordering that can be explained in one sentence: a
/// row reading `body()` sees the rows above it filled in and a row below it
/// still as its `{{name}}`, because the row below has not been worked out yet.
/// Substituting eagerly would instead freeze the body before the block ran,
/// and substituting at the end would make a value depend on a row that depends
/// on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestFacts {
    pub method: String,
    pub url: String,
    /// Name/value pairs in the order the request carries them; a name may
    /// repeat, and `header()` answers with the first match, as a server reading
    /// the request would.
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// The request's title, which is what a Postman `pm.info.requestName`
    /// becomes.
    pub name: String,
}

impl RequestFacts {
    /// What a request looks like to its own `[Gen]` block.
    ///
    /// The body is [`crate::hurl::entry::HurlEntry::body_wire`] -- what
    /// actually goes on the wire -- so a signature computed over `body()`
    /// signs the bytes the server will hash, not the user's JSON comments.
    /// Disabled header rows are left out for the same reason: they are not
    /// sent.
    pub fn of(entry: &crate::hurl::HurlEntry) -> Self {
        Self {
            method: entry.method.clone(),
            url: entry.url.clone(),
            headers: entry
                .headers
                .iter()
                .filter(|h| h.enabled)
                .map(|h| (h.key.clone(), h.value.clone()))
                .collect(),
            body: entry.body_wire().unwrap_or_default().into_owned(),
            name: entry.title.clone(),
        }
    }

    /// The URL's path: everything after the host and before the `?`. Empty if
    /// the URL is still a bare `{{base}}` -- a template we cannot parse is not
    /// an error here, it is simply a URL that has no path *yet*.
    pub fn path(&self, url: &str) -> String {
        let after_scheme = match url.find("://") {
            Some(i) => &url[i + 3..],
            None => url,
        };
        let end = after_scheme.find(['?', '#']).unwrap_or(after_scheme.len());
        match after_scheme[..end].find('/') {
            Some(i) => after_scheme[..end][i..].to_string(),
            None => String::new(),
        }
    }

    /// The URL's query string, without the `?`. Empty when there is none.
    pub fn query(&self, url: &str) -> String {
        match url.find('?') {
            Some(i) => {
                let rest = &url[i + 1..];
                rest[..rest.find('#').unwrap_or(rest.len())].to_string()
            }
            None => String::new(),
        }
    }
}

pub trait GenSource {
    /// Now, as a Unix timestamp in seconds, and the nanosecond part.
    fn now(&self) -> (i64, u32);
    /// Fill `buf` with cryptographically-unpredictable bytes.
    fn fill_random(&self, buf: &mut [u8]);
    /// The next value of the named counter, starting at 1.
    fn counter(&self, name: &str) -> u64;
    /// The request this block belongs to, when there is one.
    ///
    /// Defaulted to `None` so every existing source -- and every test fixture
    /// that only had to answer for time, randomness and counters -- keeps
    /// working: a block evaluated with no request behind it (the editor's live
    /// check, a unit test) reports `NoRequest` for these functions rather than
    /// inventing an empty one, which would quietly sign the wrong thing.
    fn request(&self) -> Option<&RequestFacts> {
        None
    }
}

/// Why a generator couldn't be evaluated. Every variant names the row, because
/// a request may declare several and "one of them is wrong" is not a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenError {
    /// The row has a name but nothing to work out yet.
    ///
    /// Its own variant rather than a parse failure: an expression the user has
    /// not written yet is not a mistyped one, and "can't read the expression
    /// (expression is empty)" said the same thing twice while sounding like the
    /// editor had failed at something.
    Empty { name: String },
    /// The expression didn't parse. Carries the offending fragment.
    Syntax { name: String, detail: String },
    /// No such function.
    UnknownFunction { name: String, function: String },
    /// A function that reads the request was used where there is no request to
    /// read: the wizard's live check, or a `[Gen]` block evaluated on its own.
    ///
    /// Its own variant rather than an empty answer, because the functions it
    /// guards are the ones used to *sign* a request -- an `hmac_sha256` over a
    /// silently empty body is a signature that looks fine and authorises
    /// nothing.
    NoRequest { name: String, function: String },
    /// A function called with the wrong number of arguments.
    Arity {
        name: String,
        function: String,
        expected: String,
        got: usize,
    },
    /// An argument was the wrong shape — a number where text was needed, or a
    /// number that doesn't fit.
    BadArgument {
        name: String,
        function: String,
        detail: String,
    },
    /// A bare identifier that nothing defines.
    UndefinedReference { name: String, reference: String },
    /// A row referring to itself, directly or through others.
    Cycle { name: String },
    /// An expression with nothing to call it. The name is the whole point of a
    /// generated row -- it is how the value reaches the request -- so a row
    /// without one computes something nothing can ever ask for. It carries no
    /// name for the obvious reason, and is the one variant whose message
    /// cannot name the row.
    NameMissing,
    /// A name Hurl could never carry in a `{{name}}`: anything outside letters,
    /// digits, `_` and `-` (see [`crate::hurl::is_variable_name`]). Such a row
    /// used to evaluate perfectly happily into a variable no placeholder could
    /// name.
    NameInvalid { name: String },
    /// The same name given to two rows in one block. Both used to evaluate,
    /// with the later silently overwriting the earlier -- so which value the
    /// request sent depended on the order of two rows that looked independent.
    NameDuplicate { name: String },
    /// A row whose value depends on an earlier row that failed. Its own
    /// variant because the alternative reads as a second, invented mistake:
    /// the earlier row is not yet bound, which looks exactly like a row
    /// referring to something below it, so a block with one typo used to
    /// report a typo *and* a cycle -- and the cycle was the louder claim.
    FailedDependency { name: String, reference: String },
}

impl GenError {
    /// The generator row the error belongs to.
    ///
    /// Only the tests need this — the status line renders each variant in full,
    /// naming the row as part of the sentence — but it is the natural accessor
    /// for the enum, so it stays rather than being open-coded in every test.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn row(&self) -> &str {
        match self {
            GenError::Empty { name }
            | GenError::Syntax { name, .. }
            | GenError::UnknownFunction { name, .. }
            | GenError::NoRequest { name, .. }
            | GenError::Arity { name, .. }
            | GenError::BadArgument { name, .. }
            | GenError::UndefinedReference { name, .. }
            | GenError::FailedDependency { name, .. }
            | GenError::NameInvalid { name }
            | GenError::NameDuplicate { name }
            | GenError::Cycle { name } => name,
            // The row has no name; that is what is wrong with it.
            GenError::NameMissing => "",
        }
    }
}

/// A parsed generator expression.
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// A literal string.
    Text(String),
    /// A literal number, kept as written so `timestamp(-30)` and `random_int`
    /// bounds stay exact and a value used as text reads back as typed.
    Number(String),
    /// A bare name: a variable reference.
    Reference(String),
    /// A function call.
    Call { function: String, args: Vec<Expr> },
}

// ── Parsing ─────────────────────────────────────────────────────────────
//
// Hand-written rather than built on `nom` (which the PaperTrail parser uses):
// the grammar is four productions with no ambiguity, and the errors this needs
// to give — "unknown function", "wrong number of arguments" — are semantic ones
// raised after parsing, not parse failures. A combinator stack would add a
// translation layer between its errors and those without removing any work.

/// The deepest a `call(call(call(…)))` may nest before parsing gives up with a
/// normal error.
///
/// Parsing, [`check`] and [`eval`] are all mutually recursive over nesting, on
/// a stack of a few MiB, so a long enough line — a few thousand `concat(` —
/// overflows it. A stack overflow is `SIGABRT`, not a panic: nothing catches
/// it, the whole process (and, since [`check`] runs on every keystroke, the
/// whole app) dies, and the "salvage what parses" recovery never runs. A limit
/// here is the one choke point that protects all three, because neither `check`
/// nor `eval` can be handed a tree the parser refused to build. The value is
/// far below where the stack is at risk and far above any real signing
/// expression (`base64(hmac_sha256(k, concat(a, b)))` is four deep).
const MAX_DEPTH: usize = 256;

/// Why a `{{name}}` inside a generator expression is refused, worded as the fix.
///
/// Everywhere else in PaperBoy -- a URL, a header, a body, an assert -- a
/// variable is written `{{name}}`, so reaching for the braces here is the
/// natural mistake rather than a careless one. But an expression is not a
/// template: a name is already a name, and `"{{SECRET}}"` is a perfectly good
/// string literal, so accepting it would sign the eight characters `{{SECRET}}`
/// and return a signature that is the right length, entirely plausible to look
/// at, and rejected with the same `401` as a wrong secret. That is the exact
/// failure this whole block exists to prevent, so the braces are a parse error
/// at the moment they are typed instead.
///
/// Substituting them instead was the other option, and is rejected because it
/// would give one thing two spellings and quietly bypass the ordering rules
/// that `eval` applies to a reference -- a `{{row_below}}` would find an
/// environment variable of the same name rather than reporting the cycle.
fn braces_fault(text: &str) -> String {
    match braced_name(text) {
        Some(name) if !name.is_empty() => {
            format!("write `{name}`, not `{{{{{name}}}}}`: an expression names a variable directly")
        }
        _ => "a variable is named directly here, not written in `{{ }}`".to_string(),
    }
}

/// The name inside the first `{{ }}` of `text`, if it has a complete one.
fn braced_name(text: &str) -> Option<&str> {
    let rest = &text[text.find("{{")? + 2..];
    Some(rest[..rest.find("}}")?].trim())
}

struct Parser<'a> {
    rest: &'a str,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser { rest: src }
    }

    fn skip_space(&mut self) {
        self.rest = self.rest.trim_start();
    }

    /// Parse a whole expression and require the input to be exhausted, so
    /// trailing rubbish is an error rather than being quietly ignored — the
    /// mistake `hurl_core`'s own placeholder parser makes, and the one this
    /// whole feature exists to work around.
    fn parse_all(mut self) -> Result<Expr, String> {
        let expr = self.expr(0)?;
        self.skip_space();
        if !self.rest.is_empty() {
            return Err(format!("unexpected `{}`", self.rest.trim()));
        }
        Ok(expr)
    }

    fn expr(&mut self, depth: usize) -> Result<Expr, String> {
        // Bound the recursion before descending, so a hostile line is a normal
        // error rather than a stack overflow (see [`MAX_DEPTH`]). `depth` counts
        // *nesting*, not siblings: each argument of a call is parsed one level
        // deeper, but the arguments of the same call are all at the same level.
        if depth > MAX_DEPTH {
            return Err("expression nests too deeply".to_string());
        }
        self.skip_space();
        match self.rest.chars().next() {
            None => Err("expression is empty".to_string()),
            Some('"') => self.string(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) if is_name_char(c) => self.ident_or_call(depth),
            Some('{') => Err(braces_fault(self.rest)),
            Some(c) => Err(format!("unexpected `{c}`")),
        }
    }

    fn string(&mut self) -> Result<Expr, String> {
        let mut out = String::new();
        let mut chars = self.rest.char_indices();
        chars.next(); // the opening quote
        // Whether an unescaped `{{` has been seen: a placeholder inside a
        // string is refused (see `braces_fault`), but `\{` is how a string that
        // really does want a brace says so, and an escaped one must not trip
        // the check.
        let mut braced = false;
        let mut prev_open_brace = false;
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    self.rest = &self.rest[i + 1..];
                    if braced {
                        return Err(braces_fault(&out));
                    }
                    return Ok(Expr::Text(out));
                }
                '\\' => {
                    prev_open_brace = false;
                    match chars.next() {
                        Some((_, 'n')) => out.push('\n'),
                        Some((_, 't')) => out.push('\t'),
                        Some((_, 'r')) => out.push('\r'),
                        Some((_, '"')) => out.push('"'),
                        Some((_, '\\')) => out.push('\\'),
                        // The way out of the rule below, for the rare string
                        // that is meant to contain a placeholder rather than
                        // stand in for one.
                        Some((_, '{')) => out.push('{'),
                        Some((_, other)) => return Err(format!("unknown escape `\\{other}`")),
                        None => return Err("string ends in a backslash".to_string()),
                    }
                }
                _ => {
                    braced |= c == '{' && prev_open_brace;
                    prev_open_brace = c == '{';
                    out.push(c);
                }
            }
        }
        Err("unterminated string".to_string())
    }

    fn number(&mut self) -> Result<Expr, String> {
        let end = self
            .rest
            .char_indices()
            .position(|(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-')))
            .unwrap_or(self.rest.len());
        let (num, rest) = self.rest.split_at(end);
        if num == "-" {
            return Err("`-` is not a number".to_string());
        }
        self.rest = rest;
        Ok(Expr::Number(num.to_string()))
    }

    fn ident_or_call(&mut self, depth: usize) -> Result<Expr, String> {
        let end = self
            .rest
            .find(|c: char| !is_name_char(c))
            .unwrap_or(self.rest.len());
        let (name, rest) = self.rest.split_at(end);
        self.rest = rest;
        let name = name.to_string();
        self.skip_space();
        if !self.rest.starts_with('(') {
            return Ok(Expr::Reference(name));
        }
        self.rest = &self.rest[1..];
        let mut args = Vec::new();
        self.skip_space();
        if self.rest.starts_with(')') {
            self.rest = &self.rest[1..];
            return Ok(Expr::Call {
                function: name,
                args,
            });
        }
        loop {
            args.push(self.expr(depth + 1)?);
            self.skip_space();
            match self.rest.chars().next() {
                Some(',') => self.rest = &self.rest[1..],
                Some(')') => {
                    self.rest = &self.rest[1..];
                    return Ok(Expr::Call {
                        function: name,
                        args,
                    });
                }
                Some(c) => return Err(format!("expected `,` or `)`, found `{c}`")),
                None => return Err(format!("`{name}(` is never closed")),
            }
        }
    }
}

/// The characters a function name or variable reference may contain — the same
/// set Hurl carries in a `{{name}}`, since a generator's *name* has to be
/// referenceable and its arguments name variables that must equally be.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

// ── The outside world ───────────────────────────────────────────────────

/// The real clock, the real random source, and the process's named counters.
///
/// `SystemSource` is deliberately cheap to build — a fresh one is made for
/// almost every send, because the clock and the random source are stateless and
/// re-reading them each time is exactly right. The counter is the exception: it
/// must give "the next value of the named counter" *across* sends, or
/// `counter("page")` returns `1` for ever and can never count a paginated
/// crawl. So the counter state does not live on the instance — it lives in one
/// process-wide table (`process_counters`) that every `SystemSource` shares.
///
/// Why a process global rather than session state: the value is a live
/// sequence, not a setting. It must never reach `state.json` (it names no
/// secret, but persisting it would make a reloaded session silently resume a
/// half-finished crawl), and it wants to be shared by every collection open in
/// the process, which a global is and a per-collection field is not. It resets
/// on restart, which is the documented "starting at 1".
#[derive(Default)]
pub struct SystemSource {
    request: Option<RequestFacts>,
}

impl SystemSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// The same source, able to answer for the request being sent.
    pub fn for_request(facts: RequestFacts) -> Self {
        Self {
            request: Some(facts),
        }
    }
}

/// The one table of named counters shared by every [`SystemSource`] in the
/// process. In memory only, never serialised (see the type's own note).
fn process_counters() -> &'static std::sync::Mutex<HashMap<String, u64>> {
    static COUNTERS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    COUNTERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

impl GenSource for SystemSource {
    fn now(&self) -> (i64, u32) {
        let now = chrono::Utc::now();
        (now.timestamp(), now.timestamp_subsec_nanos())
    }

    fn fill_random(&self, buf: &mut [u8]) {
        // A nonce or a key that silently came back all-zeroes would produce a
        // signature that looks fine and is reproducible by anyone, so a failure
        // here is fatal rather than papered over with a fallback.
        getrandom::fill(buf).expect("the operating system's random source");
    }

    fn request(&self) -> Option<&RequestFacts> {
        self.request.as_ref()
    }

    fn counter(&self, name: &str) -> u64 {
        let mut counters = process_counters().lock().unwrap_or_else(|e| e.into_inner());
        let next = counters.entry(name.to_string()).or_insert(0);
        *next += 1;
        *next
    }
}

/// The same world as [`SystemSource`], except that a counter is *read* rather
/// than advanced.
///
/// A block is evaluated twice per send: once before the request goes out, to
/// find out whether anything in it is broken (see
/// `request::describe_generator_errors`), and once for real. Every fault that
/// check reports is deterministic, so the dry run may use any values it likes
/// — but `counter` is not a value, it is a position in a sequence, and running
/// the block to ask a question moved it. `counter("page")` went 1, 3, 5.
///
/// Peeking keeps the answer honest (the dry run sees the number the real run
/// is about to produce) without laying claim to it.
pub struct DryRunSource;

impl GenSource for DryRunSource {
    fn now(&self) -> (i64, u32) {
        SystemSource::new().now()
    }

    fn fill_random(&self, buf: &mut [u8]) {
        SystemSource::new().fill_random(buf)
    }

    fn counter(&self, name: &str) -> u64 {
        let counters = process_counters().lock().unwrap_or_else(|e| e.into_inner());
        counters.get(name).copied().unwrap_or(0) + 1
    }
}

// ── Evaluation ──────────────────────────────────────────────────────────

/// Everything wrong with the *names* in a block, and which rows must therefore
/// not be evaluated.
///
/// Split out because [`expand`] and [`check`] have to agree to the letter: one
/// runs on send and the other in the editor, and a row the editor calls fine
/// but the send refuses is a request that mysteriously won't go with a message
/// naming no row. The expression checks are already shared that way; the name
/// checks are shared the same way.
///
/// A duplicate refuses the *later* row and keeps the earlier one, which is the
/// reading order and the only one that doesn't depend on where the eye starts.
/// Nothing is added to the failed set: a row named `my name` cannot be
/// referenced by any expression in the first place (a reference is parsed with
/// the same character rule), and for a duplicate the first row's value is
/// perfectly good.
fn name_faults(rows: &[(String, String)]) -> (Vec<GenError>, Vec<bool>) {
    let mut errors = Vec::new();
    let mut refused = vec![false; rows.len()];
    let mut seen: Vec<&str> = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    for (i, (name, source)) in rows.iter().enumerate() {
        let n = name.trim();
        // A wholly blank row is the editor waiting for input, not a mistake --
        // the same rule `expand` and `check` already apply to the expression.
        if n.is_empty() && source.trim().is_empty() {
            continue;
        }
        refused[i] = true;
        if n.is_empty() {
            errors.push(GenError::NameMissing);
        } else if !crate::hurl::is_variable_name(n) {
            errors.push(GenError::NameInvalid {
                name: n.to_string(),
            });
        } else if seen.contains(&n) {
            // Said once per name however many times it is repeated: the report
            // is about a name, and three copies of one sentence read as three
            // separate problems.
            if !reported.contains(&n) {
                reported.push(n);
                errors.push(GenError::NameDuplicate {
                    name: n.to_string(),
                });
            }
        } else {
            seen.push(n);
            refused[i] = false;
        }
    }
    (errors, refused)
}

/// Evaluate a request's `# [Gen]` rows and bind each result into `vars`, in
/// declaration order so a row can build on the ones above it.
///
/// Binding into the *same* map the request is substituted with is what keeps
/// the preview and the wire honest: `resolve_entry` renders the preview from
/// this map and `run_hurl` builds Hurl's `VariableSet` from it, so there is no
/// second code path to keep in step.
///
/// A row that fails is left unbound rather than bound to something plausible.
/// Its `{{name}}` then stays visible in the preview and the run is refused, in
/// preference to sending a request signed with an empty string.
///
/// Returns one error per failed row; the rest still evaluate, so a user fixing
/// a block sees every problem in it rather than the first.
pub fn expand(
    rows: &[(String, String)],
    vars: &mut HashMap<String, String>,
    src: &dyn GenSource,
) -> Vec<GenError> {
    let declared: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    // Names first: a row nothing can name, or a name two rows are fighting
    // over, is wrong whatever its expression works out to.
    let (mut errors, refused) = name_faults(rows);
    let mut done: Vec<&str> = Vec::new();
    // Rows that were reached and failed, kept apart from rows not reached yet
    // so a row reading one of them is told what actually happened rather than
    // being accused of a cycle (see `GenError::FailedDependency`).
    let mut failed: Vec<&str> = Vec::new();

    for (i, (name, source)) in rows.iter().enumerate() {
        if refused[i] {
            continue;
        }
        // A wholly blank row is a row the editor is still waiting on, not a
        // mistake — exactly as [`check`] treats it. The two must agree: `check`
        // runs in the editor while `expand` runs on send, and a row `check`
        // calls fine but `expand` rejects would make a request silently
        // unsendable with a message that names no row.
        if name.trim().is_empty() && source.trim().is_empty() {
            continue;
        }
        // A named row with nothing in it is reported the way the editor reports
        // it, for the same reason: the two must say the same thing about the
        // same block.
        if source.trim().is_empty() {
            errors.push(GenError::Empty { name: name.clone() });
            continue;
        }
        let expr = match Parser::new(source).parse_all() {
            Ok(e) => e,
            Err(detail) => {
                errors.push(GenError::Syntax {
                    name: name.clone(),
                    detail,
                });
                continue;
            }
        };
        match eval(&expr, name, vars, &declared, &done, &failed, src) {
            Ok(value) => {
                vars.insert(name.clone(), value);
                done.push(name.as_str());
            }
            Err(e) => {
                errors.push(e);
                failed.push(name.as_str());
            }
        }
    }
    errors
}

/// Everything wrong with a block that can be known without running it: rows
/// that don't parse, functions that don't exist, and calls with the wrong
/// number of arguments.
///
/// Deliberately *not* undefined references: an editor is often open on a
/// request whose environment isn't loaded, and flagging `{{ api_key }}` as a
/// fault there would train the user to ignore the one part of this that is
/// always a real mistake.
pub fn check(rows: &[(String, String)]) -> Vec<GenError> {
    fn walk(expr: &Expr, row: &str, out: &mut Vec<GenError>) {
        match expr {
            Expr::Text(_) | Expr::Number(_) => {}
            Expr::Reference(name) => {
                // A bare name is either a zero-argument call or a reference to
                // a variable, and only the first can be checked here.
                if let Some(f) = function(name)
                    && f.min_args > 0
                {
                    out.push(GenError::Arity {
                        name: row.to_string(),
                        function: name.clone(),
                        expected: expected_arity(f),
                        got: 0,
                    });
                }
            }
            Expr::Call {
                function: fname,
                args,
            } => {
                match function(fname) {
                    None => out.push(GenError::UnknownFunction {
                        name: row.to_string(),
                        function: fname.clone(),
                    }),
                    Some(f) => {
                        if args.len() < f.min_args || f.max_args.is_some_and(|m| args.len() > m) {
                            out.push(GenError::Arity {
                                name: row.to_string(),
                                function: fname.clone(),
                                expected: expected_arity(f),
                                got: args.len(),
                            });
                        }
                    }
                }
                for a in args {
                    walk(a, row, out);
                }
            }
        }
    }

    let (mut out, refused) = name_faults(rows);
    for (i, (name, source)) in rows.iter().enumerate() {
        if refused[i] || (name.trim().is_empty() && source.trim().is_empty()) {
            continue;
        }
        // A row that has been named and not yet filled in is half-written, not
        // wrong: say what is missing rather than reporting a parse failure.
        if source.trim().is_empty() {
            out.push(GenError::Empty { name: name.clone() });
            continue;
        }
        match Parser::new(source).parse_all() {
            Err(detail) => out.push(GenError::Syntax {
                name: name.clone(),
                detail,
            }),
            Ok(expr) => walk(&expr, name, &mut out),
        }
    }
    out
}

/// How an arity reads in a message: the same words [`call`] uses.
fn expected_arity(f: &GenFunction) -> String {
    match (f.min_args, f.max_args) {
        (lo, Some(hi)) if lo == hi => lo.to_string(),
        (lo, Some(hi)) => format!("{lo} or {hi}"),
        (lo, None) => format!("{lo} or more"),
    }
}

/// Evaluate one expression. `declared` is every generator name in the block and
/// `done` those already evaluated, which is how a row referring to itself or to
/// a row below it is told apart from one referring to an environment variable
/// that happens to share the name.
fn eval(
    expr: &Expr,
    row: &str,
    vars: &HashMap<String, String>,
    declared: &[&str],
    done: &[&str],
    failed: &[&str],
    src: &dyn GenSource,
) -> Result<String, GenError> {
    match expr {
        Expr::Text(t) => Ok(t.clone()),
        Expr::Number(n) => Ok(n.clone()),
        Expr::Reference(name) => {
            // A bare function name is a call with no arguments, so a block can
            // read `nonce = random_hex(32)` / `stamp = timestamp` rather than
            // insisting on empty parentheses. The function set wins over a
            // variable of the same name: it is small, fixed and documented,
            // whereas resolving it by whichever happens to exist would make the
            // meaning of a row depend on the loaded environment.
            if is_function(name.as_str()) {
                return call(name, &[], row, vars, src);
            }
            // An earlier row that was tried and failed is a *consequence*,
            // not a second mistake: say which row is missing so the reader
            // fixes the one that is actually wrong.
            if failed.contains(&name.as_str()) {
                return Err(GenError::FailedDependency {
                    name: row.to_string(),
                    reference: name.clone(),
                });
            }
            if declared.contains(&name.as_str()) && !done.contains(&name.as_str()) {
                // Deliberately checked before `vars`: silently falling back to
                // an environment variable of the same name would make a
                // mis-ordered block *work*, differently, and only sometimes.
                return Err(GenError::Cycle {
                    name: row.to_string(),
                });
            }
            vars.get(name)
                .cloned()
                .ok_or_else(|| GenError::UndefinedReference {
                    name: row.to_string(),
                    reference: name.clone(),
                })
        }
        Expr::Call { function, args } => {
            // The name is checked before the arguments are evaluated. The other
            // way round, `hmac_sha526(key, body)` complains that nothing defines
            // `key` — true, but it sends the user looking at their environment
            // for a fault that is a typo in the function name.
            if !is_function(function.as_str()) {
                return Err(GenError::UnknownFunction {
                    name: row.to_string(),
                    function: function.clone(),
                });
            }
            let mut values = Vec::with_capacity(args.len());
            for a in args {
                values.push(eval(a, row, vars, declared, done, failed, src)?);
            }
            call(function, &values, row, vars, src)
        }
    }
}

/// Apply a generator function to its already-evaluated arguments.
fn call(
    function: &str,
    args: &[String],
    row: &str,
    vars: &HashMap<String, String>,
    src: &dyn GenSource,
) -> Result<String, GenError> {
    let arity = |expected: &str, ok: bool| -> Result<(), GenError> {
        if ok {
            Ok(())
        } else {
            Err(GenError::Arity {
                name: row.to_string(),
                function: function.to_string(),
                expected: expected.to_string(),
                got: args.len(),
            })
        }
    };
    let bad = |detail: String| GenError::BadArgument {
        name: row.to_string(),
        function: function.to_string(),
        detail,
    };
    // A size or offset written into a request, so it is read strictly: a
    // silently clamped length makes a nonce shorter than the author asked for.
    //
    // The message names the argument rather than quoting its value: an argument
    // here can be the resolved form of a secret (`random_hex(API_SECRET)`), and
    // this text reaches the status bar, the CLI's stderr and CI logs — the one
    // place in this feature that must never carry a secret in the clear (it is
    // why the runner is handed `variables.secrets()`).
    let count = |s: &String| -> Result<usize, GenError> {
        s.parse::<usize>()
            .map_err(|_| bad("the length must be a whole number".to_string()))
    };

    match function {
        // ── Time ────────────────────────────────────────────────────────
        "timestamp" => {
            arity("0 or 1", args.len() <= 1)?;
            let offset = match args.first() {
                None => 0,
                Some(a) => a
                    .parse::<i64>()
                    .map_err(|_| bad("the offset must be a whole number of seconds".to_string()))?,
            };
            // Checked so a huge offset is a normal `BadArgument`, not a debug
            // panic / release wraparound: the panic would land on the send
            // thread, which then never clears `loading` and the UI spins for
            // ever.
            let stamp = src
                .now()
                .0
                .checked_add(offset)
                .ok_or_else(|| bad("the offset is too large".to_string()))?;
            Ok(stamp.to_string())
        }
        "timestamp_ms" => {
            arity("0", args.is_empty())?;
            let (secs, nanos) = src.now();
            Ok((secs * 1000 + i64::from(nanos / 1_000_000)).to_string())
        }
        "iso8601" => {
            arity("0", args.is_empty())?;
            Ok(utc(src).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        }
        "date" => {
            arity("1", args.len() == 1)?;
            Ok(utc(src).format(&args[0]).to_string())
        }

        // ── Identity and randomness ─────────────────────────────────────
        "uuid" => {
            arity("0", args.is_empty())?;
            Ok(uuid::Uuid::new_v4().to_string())
        }
        "counter" => {
            arity("1", args.len() == 1)?;
            Ok(src.counter(&args[0]).to_string())
        }
        "random_int" => {
            arity("2", args.len() == 2)?;
            let lo = args[0]
                .parse::<i64>()
                .map_err(|_| bad("the low bound must be a whole number".to_string()))?;
            let hi = args[1]
                .parse::<i64>()
                .map_err(|_| bad("the high bound must be a whole number".to_string()))?;
            if lo > hi {
                return Err(bad(format!("{lo} is greater than {hi}")));
            }
            Ok(random_int(lo, hi, src).to_string())
        }
        "random_hex" => {
            arity("1", args.len() == 1)?;
            let n = count(&args[0])?;
            let mut bytes = vec![0u8; n.div_ceil(2)];
            src.fill_random(&mut bytes);
            let mut out = to_hex(&bytes);
            out.truncate(n);
            Ok(out)
        }
        "random_alnum" => {
            arity("1", args.len() == 1)?;
            Ok(random_from(
                count(&args[0])?,
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                src,
            ))
        }
        "random_base64" => {
            arity("1", args.len() == 1)?;
            let mut bytes = vec![0u8; count(&args[0])?];
            src.fill_random(&mut bytes);
            Ok(b64(&bytes, false))
        }

        // ── Encoding ────────────────────────────────────────────────────
        "base64" => {
            arity("1", args.len() == 1)?;
            Ok(b64(args[0].as_bytes(), false))
        }
        "base64url" => {
            arity("1", args.len() == 1)?;
            Ok(b64(args[0].as_bytes(), true))
        }
        "base64_decode" => {
            arity("1", args.len() == 1)?;
            use base64::Engine;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(args[0].as_bytes())
                .map_err(|e| bad(format!("not valid base64 ({e})")))?;
            String::from_utf8(raw).map_err(|_| bad("decodes to bytes that aren't text".to_string()))
        }
        "hex" => {
            arity("1", args.len() == 1)?;
            Ok(to_hex(args[0].as_bytes()))
        }
        "urlencode" => {
            arity("1", args.len() == 1)?;
            Ok(percent_encode(&args[0]))
        }
        "urldecode" => {
            arity("1", args.len() == 1)?;
            percent_decode(&args[0]).map_err(bad)
        }
        "json_string" => {
            arity("1", args.len() == 1)?;
            Ok(serde_json::Value::String(args[0].clone()).to_string())
        }
        // Reaching into a JSON document the block already has in hand -- a
        // response an earlier request captured whole, or this request's own
        // `body()`. A `[Captures]` row is the right tool when the value comes
        // straight from a response; this is for the cases a capture cannot
        // reach, which is anything that has to be *computed* from the value:
        // signing part of a payload, or building the next request's body out of
        // pieces of the last one's.
        "jsonpath" => {
            arity("2", args.len() == 2)?;
            let doc: serde_json::Value =
                serde_json::from_str(&args[0]).map_err(|e| bad(format!("not valid JSON ({e})")))?;
            json_path(&doc, &args[1]).map_err(bad)
        }

        // ── Hashes and signatures ───────────────────────────────────────
        // The digest is bytes; the request needs text. Which text is not a
        // detail — a signature in the wrong encoding is the right length, looks
        // entirely plausible, and is rejected with the same 401 as a wrong
        // secret. So the encoding is part of the name rather than a default to
        // be discovered: bare is hex (what `sha256sum` and CryptoJS's
        // `toString()` produce, which is what a ported script expects), `_b64`
        // is standard padded Base64 (what Twilio, AWS and friends want).
        "md5" | "sha1" | "sha256" | "sha512" | "md5_b64" | "sha1_b64" | "sha256_b64"
        | "sha512_b64" | "md5_b64url" | "sha1_b64url" | "sha256_b64url" | "sha512_b64url" => {
            arity("1", args.len() == 1)?;
            let (alg, as_b64) = split_encoding(function);
            Ok(encode_digest(&hash_bytes(alg, args[0].as_bytes()), as_b64))
        }
        "hmac_sha1" | "hmac_sha256" | "hmac_sha512" | "hmac_sha1_b64" | "hmac_sha256_b64"
        | "hmac_sha512_b64" | "hmac_sha1_b64url" | "hmac_sha256_b64url" | "hmac_sha512_b64url" => {
            arity("2", args.len() == 2)?;
            let (alg, as_b64) = split_encoding(function);
            let alg = alg.strip_prefix("hmac_").expect("matched an hmac_ name");
            // Key first, message second — the order every library and every
            // API's documentation uses. Swapping them yields a signature that
            // is well-formed and wrong, which is why it is worth stating.
            let mac = hmac_bytes(alg, args[0].as_bytes(), args[1].as_bytes());
            Ok(encode_digest(&mac, as_b64))
        }

        // ── Text ────────────────────────────────────────────────────────
        "concat" => Ok(args.concat()),
        "upper" => {
            arity("1", args.len() == 1)?;
            Ok(args[0].to_uppercase())
        }
        "lower" => {
            arity("1", args.len() == 1)?;
            Ok(args[0].to_lowercase())
        }
        "trim" => {
            arity("1", args.len() == 1)?;
            Ok(args[0].trim().to_string())
        }
        // Taking *part* of a string is what a ported script needs most often:
        // a test-case number off the end of a request name, a token out of a
        // header, an id out of a path. `n` counts from 0, and from the end
        // when negative, so "the last piece" -- JavaScript's `.pop()`, the
        // shape these scripts are written in -- is `-1` rather than a length
        // the block has no way to work out.
        "split" => {
            arity("3", args.len() == 3)?;
            let pieces: Vec<&str> = if args[1].is_empty() {
                // Splitting on nothing yields one empty piece per character in
                // Rust, which is a silent trap: an empty separator here is
                // almost always a `{{sep}}` that resolved to nothing.
                return Err(bad("the separator cannot be empty".to_string()));
            } else {
                args[0].split(args[1].as_str()).collect()
            };
            let n = args[2]
                .trim()
                .parse::<i64>()
                .map_err(|_| bad("the piece number must be a whole number".to_string()))?;
            let idx = if n < 0 { pieces.len() as i64 + n } else { n };
            // Out of range is a fault, not an empty answer: this text goes on
            // to be signed, sent or asserted against, and a silently empty
            // piece is the kind of wrong that looks like the server's fault.
            usize::try_from(idx)
                .ok()
                .and_then(|i| pieces.get(i))
                .map(|p| p.to_string())
                .ok_or_else(|| {
                    bad(format!(
                        "there is no piece {n}; the text splits into {}",
                        pieces.len()
                    ))
                })
        }
        // The escape hatch for everything `split` cannot reach. The first
        // capture group if the pattern has one -- which is how a pattern says
        // "this part" -- and otherwise the whole match.
        "regex" => {
            arity("2", args.len() == 2)?;
            let re = regex::Regex::new(&args[1])
                .map_err(|e| bad(format!("the pattern is not valid: {e}")))?;
            let caps = re
                .captures(&args[0])
                .ok_or_else(|| bad("the pattern matched nothing".to_string()))?;
            Ok(caps
                .get(1)
                .or_else(|| caps.get(0))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default())
        }

        // ── The request this block belongs to ───────────────────────────
        //
        // Every one of these reads `RequestFacts`, whose text is stored as
        // written and substituted here against the variables worked out so
        // far, so a row reading the body sees the rows above it filled in.
        // Without a request behind the block -- the editor's live check --
        // they fail rather than answer with nothing, because the whole point
        // of reading the request is to sign or record what is actually sent.
        "method" | "url" | "path" | "query" | "body" | "request_name" => {
            arity("0", args.is_empty())?;
            let facts = src.request().ok_or_else(|| GenError::NoRequest {
                name: row.to_string(),
                function: function.to_string(),
            })?;
            let fill = |t: &str| crate::environment::substitute(t, vars);
            Ok(match function {
                "method" => facts.method.to_ascii_uppercase(),
                "url" => fill(&facts.url),
                "path" => facts.path(&fill(&facts.url)),
                "query" => facts.query(&fill(&facts.url)),
                "body" => fill(&facts.body),
                // The title as typed: it is a label, not a URL, and a
                // migration that reads it back is comparing it with the
                // Postman request name it came from.
                _ => facts.name.clone(),
            })
        }
        "header" => {
            arity("1", args.len() == 1)?;
            let facts = src.request().ok_or_else(|| GenError::NoRequest {
                name: row.to_string(),
                function: function.to_string(),
            })?;
            // Case-insensitively, and the first of a repeated name wins --
            // both are how a server reads the request being described.
            // Missing is empty rather than an error: a signature over "the
            // Content-Type if there is one" is a real thing to write, and an
            // absent header is not a mistake in the block.
            Ok(facts
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(args[0].trim()))
                .map(|(_, v)| crate::environment::substitute(v, vars))
                .unwrap_or_default())
        }

        _ => Err(GenError::UnknownFunction {
            name: row.to_string(),
            function: function.to_string(),
        }),
    }
}

/// One generator function, as the editors offer it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GenFunction {
    /// The name as written in a row.
    pub name: &'static str,
    /// How to call it, argument names included, with optional arguments in
    /// square brackets. Deliberately *not* translated: these are identifiers
    /// the user types, and a translated `hmac_sha256(clé, message)` would be a
    /// call that does not work.
    pub signature: &'static str,
    /// How many arguments it must have, and at most may have (`None` for a
    /// function that takes any number). Stated here so an editor can say
    /// "takes 2" while typing rather than leaving it to the send, and checked
    /// against [`call`] by a test — two places that disagreed about arity
    /// would be worse than one.
    pub min_args: usize,
    pub max_args: Option<usize>,
    /// Complete calls to offer beside the signature, for a function whose
    /// argument is a small language of its own.
    ///
    /// `date(format)` names the argument without saying a word about what a
    /// format looks like, and the failure -- "date takes 1 arguments, not 0" --
    /// says even less. There are some forty strftime specifiers, so listing
    /// them all would be a reference manual in a dropdown; a handful of whole,
    /// working calls is what someone reaching for a date actually wants, and
    /// each one doubles as an example of the syntax for anyone who then wants
    /// something else.
    ///
    /// Untranslated for the same reason as `signature`: they are text the user
    /// is about to run.
    pub examples: &'static [&'static str],
}

/// Every generator function, for the editors' suggestions and for
/// documentation. Kept beside [`call`] so a function added there is offered
/// here, which a test enforces in both directions.
///
/// **In alphabetical order**, and a test keeps it that way. This is the order
/// both front-ends' dropdowns show, and browsing the whole list is what that
/// dropdown is for: grouped by kind it read well as source, but it left a
/// reader hunting for `sha256` with nothing to scan against. Where a related
/// set matters -- the four `random_*`, the `hmac_*` pairs -- the shared prefix
/// keeps it together anyway.
pub const FUNCTIONS: &[GenFunction] = &[
    GenFunction {
        name: "base64",
        signature: "base64(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "base64_decode",
        signature: "base64_decode(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "base64url",
        signature: "base64url(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "body",
        signature: "body()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "concat",
        signature: "concat(a, b, …)",
        min_args: 0,
        max_args: None,
        examples: &[],
    },
    GenFunction {
        name: "counter",
        signature: "counter(name)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "date",
        signature: "date(format)",
        min_args: 1,
        max_args: Some(1),
        // Dates first, then times, then the two run together: the order
        // someone scanning for "the one I mean" reads in.
        examples: &[
            r#"date("%Y-%m-%d")"#,
            r#"date("%d/%m/%Y")"#,
            r#"date("%d %b %Y")"#,
            r#"date("%Y-%m-%dT%H:%M:%SZ")"#,
            r#"date("%H:%M:%S")"#,
            r#"date("%Y%m%d%H%M%S")"#,
        ],
    },
    GenFunction {
        name: "header",
        signature: "header(name)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "hex",
        signature: "hex(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha1",
        signature: "hmac_sha1(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha1_b64",
        signature: "hmac_sha1_b64(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha1_b64url",
        signature: "hmac_sha1_b64url(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha256",
        signature: "hmac_sha256(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha256_b64",
        signature: "hmac_sha256_b64(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha256_b64url",
        signature: "hmac_sha256_b64url(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha512",
        signature: "hmac_sha512(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha512_b64",
        signature: "hmac_sha512_b64(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "hmac_sha512_b64url",
        signature: "hmac_sha512_b64url(key, message)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "iso8601",
        signature: "iso8601()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "json_string",
        signature: "json_string(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "jsonpath",
        signature: "jsonpath(text, path)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "lower",
        signature: "lower(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "md5",
        signature: "md5(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "md5_b64",
        signature: "md5_b64(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "md5_b64url",
        signature: "md5_b64url(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "method",
        signature: "method()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "path",
        signature: "path()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "query",
        signature: "query()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "random_alnum",
        signature: "random_alnum(length)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "random_base64",
        signature: "random_base64(bytes)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "random_hex",
        signature: "random_hex(length)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "random_int",
        signature: "random_int(low, high)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "regex",
        signature: "regex(text, pattern)",
        min_args: 2,
        max_args: Some(2),
        examples: &[],
    },
    GenFunction {
        name: "request_name",
        signature: "request_name()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "sha1",
        signature: "sha1(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha1_b64",
        signature: "sha1_b64(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha1_b64url",
        signature: "sha1_b64url(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha256",
        signature: "sha256(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha256_b64",
        signature: "sha256_b64(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha256_b64url",
        signature: "sha256_b64url(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha512",
        signature: "sha512(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha512_b64",
        signature: "sha512_b64(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "sha512_b64url",
        signature: "sha512_b64url(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "split",
        signature: "split(text, separator, n)",
        min_args: 3,
        max_args: Some(3),
        examples: &[],
    },
    GenFunction {
        name: "timestamp",
        signature: "timestamp([offset_seconds])",
        min_args: 0,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "timestamp_ms",
        signature: "timestamp_ms()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "trim",
        signature: "trim(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "upper",
        signature: "upper(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "url",
        signature: "url()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
    GenFunction {
        name: "urldecode",
        signature: "urldecode(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "urlencode",
        signature: "urlencode(text)",
        min_args: 1,
        max_args: Some(1),
        examples: &[],
    },
    GenFunction {
        name: "uuid",
        signature: "uuid()",
        min_args: 0,
        max_args: Some(0),
        examples: &[],
    },
];

/// The function called `name`, if there is one.
pub fn function(name: &str) -> Option<&'static GenFunction> {
    FUNCTIONS.iter().find(|f| f.name == name)
}

/// The names alone, for a lookup that does not care how a function is called.
pub fn is_function(name: &str) -> bool {
    FUNCTIONS.iter().any(|f| f.name == name)
}

/// The functions whose name begins with `prefix`, in table (alphabetical)
/// order, for a completion list. An empty prefix offers everything.
pub fn functions_starting_with(prefix: &str) -> impl Iterator<Item = &'static GenFunction> {
    let prefix = prefix.to_ascii_lowercase();
    FUNCTIONS
        .iter()
        .filter(move |f| f.name.starts_with(prefix.as_str()))
}
/// The identifier being typed at `caret` in a `[Gen]` expression.
///
/// A word here is what a function name may be made of, so the caret in
/// `concat(upper(na|me))` picks out `name` and not the whole expression. An
/// expression is not one name the way a header is: completion has to work
/// inside a call, because that is where the nesting this feature exists for
/// puts it.
///
/// The word is split at the caret rather than taken whole, because the two
/// halves mean different things. What is *before* the caret is what the user
/// has typed and so what filters the list: typing `t` in front of an existing
/// `uuid` is someone reaching for `timestamp`, but the word straddling the
/// caret is `tuuid`, which matches nothing and left the list blank exactly when
/// it was wanted. What is *after* it is text the user put the caret in front of
/// deliberately, and is offered to the accepted call as its first argument --
/// which is how `t|uuid` becomes `timestamp(uuid)` rather than losing the
/// `uuid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedWord {
    /// Where the word starts.
    pub start: usize,
    /// Where the word ends.
    pub end: usize,
    /// Where the text to wrap ends: the end of the word, or of the call that
    /// begins there — the caret in `t|base64(x)` is in front of the whole
    /// `base64(x)`, not just the name.
    pub wrap_end: usize,
    /// The typed part, before the caret. What the list filters on.
    pub prefix: String,
    /// The text from the caret to `wrap_end`: what an accepted call wraps.
    pub wrapped: String,
    /// The whole word, both sides of the caret. Not what the list filters on,
    /// but what decides there is nothing left to offer: a caret dropped in the
    /// middle of a finished `uuid` should not open a list over the row below.
    pub whole: String,
}

pub fn typed_word_at(text: &str, caret: Option<usize>) -> TypedWord {
    let chars: Vec<char> = text.chars().collect();
    let at = caret.unwrap_or(chars.len()).min(chars.len());
    let is_word = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let mut start = at;
    while start > 0 && is_word(&chars[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < chars.len() && is_word(&chars[end]) {
        end += 1;
    }
    // A name followed by its brackets is one thing to wrap, so scan the call
    // out to its matching `)`. An unbalanced tail is left alone: wrapping half
    // a call would move a bracket the user still has to close.
    let mut wrap_end = end;
    if chars.get(end) == Some(&'(') {
        let mut depth = 0usize;
        for (i, c) in chars.iter().enumerate().skip(end) {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        wrap_end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    TypedWord {
        prefix: chars[start..at].iter().collect(),
        wrapped: chars[at..wrap_end].iter().collect(),
        whole: chars[start..end].iter().collect(),
        start,
        end,
        wrap_end,
    }
}

/// The completion list to offer while `prefix` is being typed: each matching
/// function's signature, followed by any ready-made calls it offers. `None`
/// when there is nothing worth showing.
///
/// `browse` is "the user asked for the list" (Ctrl+Space, or Enter in the
/// terminal wizard) rather than "the user is typing": it is the only thing that
/// makes an empty word offer the whole catalogue, because a list that appeared
/// over an empty cell on its own would be in the way of every other way of
/// filling it in.
///
/// `whole` is the word both sides of the caret. A name already typed in full
/// and matching nothing else offers nothing: a dropdown that will not close
/// reads as the editor refusing what was typed, and a caret dropped in the
/// middle of a finished `uuid` to edit it would otherwise open a list over the
/// row below.
///
/// Shared by both front-ends deliberately. They had the same list built twice
/// from the same table, which is how the two came to disagree about when it
/// should appear.
pub fn suggestions_for_word(prefix: &str, whole: &str, browse: bool) -> Option<Vec<&'static str>> {
    if prefix.is_empty() && !browse {
        return None;
    }
    let sugs: Vec<&'static str> = functions_starting_with(prefix)
        .flat_map(|f| std::iter::once(f.signature).chain(f.examples.iter().copied()))
        .collect();
    // A function that carries ready-made example calls still has something to
    // offer once its name is complete -- `date` is the whole reason the
    // examples exist -- so "nothing left to choose" is about the rows, not the
    // name.
    let done = sugs.len() == 1 && is_function(whole);
    (!sugs.is_empty() && !done).then_some(sugs)
}

/// What accepting function `f` puts in place of the word at the caret, and how
/// far into that text the caret should then sit.
///
/// Keyed off `min_args` rather than the wording of the signature: a function
/// that needs an argument is written with *both* brackets and the caret between
/// them, so the next keystroke is the argument and the block doesn't report an
/// unclosed `(` as a fault; one that needs none is complete as its bare name,
/// which is how a block already reads `stamp = timestamp`.
pub fn completion(f: &GenFunction) -> (String, usize) {
    if f.min_args == 0 {
        (f.name.to_string(), f.name.chars().count())
    } else {
        (format!("{}()", f.name), f.name.chars().count() + 1)
    }
}

/// Whether `f` has anywhere to put text the caret was placed in front of.
///
/// Anything that takes an argument can be built *around* what is already
/// there: the caret in `|uuid` completed with `base64` means `base64(uuid)`,
/// not a `base64` where the `uuid` used to be. This asks about the arguments a
/// function *may* take rather than the ones it *must*, because the two
/// front-ends both used to ask about `min_args` -- and so threw the word away
/// for `timestamp([offset_seconds])`, whose optional argument is exactly
/// somewhere to put it. Only a function that can hold nothing (`uuid`,
/// `timestamp_ms`) replaces the word, because there is nowhere for it to go.
///
/// Shared so the two front-ends cannot drift on the question again.
pub fn can_wrap(f: &GenFunction) -> bool {
    f.max_args != Some(0)
}

/// The function a suggestion row names, whether the row is a signature or one
/// of the ready-made example calls listed under it.
pub fn function_for_suggestion(row: &str) -> Option<&'static GenFunction> {
    FUNCTIONS
        .iter()
        .find(|f| f.signature == row)
        .or_else(|| FUNCTIONS.iter().find(|f| f.examples.contains(&row)))
}

fn utc(src: &dyn GenSource) -> chrono::DateTime<chrono::Utc> {
    let (secs, nanos) = src.now();
    chrono::DateTime::from_timestamp(secs, nanos).unwrap_or_default()
}

/// Split a hash or MAC function name into the algorithm and whether its result
/// is wanted as Base64 rather than hex.
/// How a digest is written out, taken from the tail of the function's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestEncoding {
    /// Bare: lowercase hex, what `sha256sum` and CryptoJS's `toString()` give.
    Hex,
    /// `_b64`: standard padded Base64, what Twilio, AWS and friends want.
    B64,
    /// `_b64url`: URL-safe Base64 without padding -- the encoding a JWT is
    /// made of, and the reason this variant exists: `header.payload.signature`
    /// is three of these joined by dots, and the standard alphabet's `+`, `/`
    /// and `=` are all wrong in that position.
    B64Url,
}

/// `_b64url` is tried before `_b64` because the latter is its prefix.
fn split_encoding(function: &str) -> (&str, DigestEncoding) {
    if let Some(alg) = function.strip_suffix("_b64url") {
        (alg, DigestEncoding::B64Url)
    } else if let Some(alg) = function.strip_suffix("_b64") {
        (alg, DigestEncoding::B64)
    } else {
        (function, DigestEncoding::Hex)
    }
}

fn encode_digest(bytes: &[u8], how: DigestEncoding) -> String {
    match how {
        DigestEncoding::Hex => to_hex(bytes),
        DigestEncoding::B64 => b64(bytes, false),
        DigestEncoding::B64Url => b64(bytes, true),
    }
}

fn hash_bytes(alg: &str, msg: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    match alg {
        "md5" => md5::Md5::digest(msg).to_vec(),
        "sha1" => sha1::Sha1::digest(msg).to_vec(),
        "sha256" => sha2::Sha256::digest(msg).to_vec(),
        "sha512" => sha2::Sha512::digest(msg).to_vec(),
        other => unreachable!("hash_bytes called with {other}, which `call` does not dispatch"),
    }
}

fn hmac_bytes(alg: &str, key: &[u8], msg: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    // Written out per algorithm rather than generically: the bounds needed to
    // abstract over a RustCrypto digest are longer than the three lines they
    // would save, and there are exactly three.
    //
    // `new_from_slice` cannot fail here — HMAC takes a key of any length, first
    // hashing anything longer than the block size — so the error is unreachable
    // rather than something to report.
    macro_rules! mac {
        ($d:ty) => {{
            let mut m =
                hmac::Hmac::<$d>::new_from_slice(key).expect("HMAC accepts a key of any length");
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }};
    }
    match alg {
        "sha1" => mac!(sha1::Sha1),
        "sha256" => mac!(sha2::Sha256),
        "sha512" => mac!(sha2::Sha512),
        other => unreachable!("hmac_bytes called with {other}, which `call` does not dispatch"),
    }
}

fn b64(bytes: &[u8], url_safe: bool) -> String {
    use base64::Engine;
    if url_safe {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    } else {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}

/// Read a value out of a JSON document with a plain `$.a.b[0]` path.
///
/// The walk itself is [`crate::report::run::json_path_get`] -- the same one a
/// report column uses -- rather than a second implementation: the same path
/// written in two places in PaperBoy has to mean the same thing, and two
/// hand-rolled walkers that agree on the easy paths and differ on the hard ones
/// is the worst outcome available. What is added here is the *why*: a walk that
/// finds nothing comes back as `None`, and a row that failed needs to say
/// whether the document was not JSON, the path was not a path, or the value
/// simply is not there.
///
/// The notation it does not implement is refused by name, pointing at the
/// `[Captures]` row that has Hurl's full JSONPath. Refusing matters more than
/// it looks: a `$..id` that quietly picked the wrong `id` yields a value that
/// looks perfectly reasonable in the request it ends up in, which is the exact
/// kind of wrong a generator block exists to stop.
fn json_path(doc: &serde_json::Value, path: &str) -> Result<String, String> {
    let path = path.trim();
    let unsupported = |what: &str| {
        Err(format!(
            "{what} is not supported here — use a `[Captures]` row, which has \
             Hurl's full JSONPath"
        ))
    };
    if !path.starts_with('$') {
        return Err(format!("a path starts with `$`, not {path:?}"));
    }
    if path.contains("..") {
        return unsupported("recursive descent (`..`)");
    }
    if path.contains('*') {
        return unsupported("a wildcard");
    }
    // Inside brackets only: a `:` or `,` can appear perfectly legitimately in a
    // quoted key, and `[?(...)]` filters *are* supported.
    for part in path.split('[').skip(1) {
        let inside = part.split(']').next().unwrap_or_default().trim();
        if inside.starts_with('?') {
            continue;
        }
        if inside.starts_with('\'') || inside.starts_with('"') {
            continue;
        }
        if inside.contains(':') {
            return unsupported("a slice");
        }
        if inside.contains(',') {
            return unsupported("a union");
        }
    }

    // Missing is an error, not an empty answer: this text goes on to be signed,
    // sent or asserted against, and quietly nothing is the kind of wrong that
    // looks like the server's fault.
    let found = crate::report::run::json_path_get(doc, path)
        .ok_or_else(|| format!("there is nothing at {path}"))?;

    Ok(match found {
        // A string is its text, not its JSON spelling: a row reading `$.token`
        // wants the token, not `"the-token"` with the quotes still on.
        serde_json::Value::String(s) => s,
        // `null` is refused rather than rendered: "null" is four plausible
        // characters to sign or send, and never what was meant.
        serde_json::Value::Null => return Err(format!("the value at {path} is null")),
        // An object or array comes back as compact JSON -- the only sensible
        // text for it, and what a script hashing part of a payload wants.
        // Canonicalisation is the author's, as everywhere else in a block.
        other => other.to_string(),
    })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A uniform value in `lo..=hi`, by rejection — the modulo shortcut skews the
/// low end of the range, which for a range used as a test-data bound is
/// invisible and wrong.
fn random_int(lo: i64, hi: i64, src: &dyn GenSource) -> i64 {
    let span = (hi as i128 - lo as i128 + 1) as u128;
    if span == 1 {
        return lo;
    }
    let limit = u128::MAX - (u128::MAX % span) - 1;
    loop {
        let mut buf = [0u8; 16];
        src.fill_random(&mut buf);
        let draw = u128::from_le_bytes(buf);
        if draw <= limit {
            return (lo as i128 + (draw % span) as i128) as i64;
        }
    }
}

/// `n` characters drawn uniformly from `alphabet`, again by rejection.
fn random_from(n: usize, alphabet: &[u8], src: &dyn GenSource) -> String {
    let len = alphabet.len() as u8;
    let limit = u8::MAX - (u8::MAX % len) - 1;
    let mut out = String::with_capacity(n);
    let mut buf = [0u8; 64];
    while out.len() < n {
        src.fill_random(&mut buf);
        for b in buf {
            if b <= limit {
                out.push(alphabet[(b % len) as usize] as char);
                if out.len() == n {
                    break;
                }
            }
        }
    }
    out
}

/// Percent-encode everything outside RFC 3986's unreserved set. Deliberately
/// strict: this is used to build values placed into URLs and signing strings,
/// where an under-encoded `&` or `=` changes what is being signed.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String, String> {
    let raw = s.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = raw
                .get(i + 1..i + 3)
                .ok_or_else(|| "ends in an incomplete `%` escape".to_string())?;
            let hex = std::str::from_utf8(hex).map_err(|_| "invalid `%` escape".to_string())?;
            out.push(
                u8::from_str_radix(hex, 16).map_err(|_| format!("`%{hex}` is not a hex escape"))?,
            );
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "decodes to bytes that aren't text".to_string())
}

#[cfg(test)]
mod tests {
    /// The table is the order both dropdowns show, and it is alphabetical --
    /// so a function added to the end of the list, where a new entry naturally
    /// goes, is caught here rather than by a reader wondering why `zzz` sits
    /// after `trim`.
    #[test]
    fn the_function_table_is_in_alphabetical_order() {
        let names: Vec<&str> = FUNCTIONS.iter().map(|f| f.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "FUNCTIONS is out of alphabetical order");
    }

    /// The two halves of the word at the caret mean different things: what is
    /// typed filters the list, what follows is what a call would wrap.
    #[test]
    fn a_word_is_split_at_the_caret() {
        let w = typed_word_at("tuuid", Some(1));
        assert_eq!(w.prefix, "t", "only the typed part filters the list");
        assert_eq!(w.wrapped, "uuid");
        assert_eq!((w.start, w.end, w.wrap_end), (0, 5, 5));
    }

    /// A name followed by its brackets is one thing to wrap: the caret in
    /// `b|sha256(x)` is in front of the whole call, not just the name.
    #[test]
    fn the_call_after_the_caret_is_wrapped_whole() {
        let w = typed_word_at("bsha256(md5(x))", Some(1));
        assert_eq!(w.prefix, "b");
        assert_eq!(w.wrapped, "sha256(md5(x))");
        assert_eq!(w.wrap_end, 15);
    }

    /// An unbalanced tail is left alone -- wrapping half a call would move a
    /// bracket the user still has to close.
    #[test]
    fn an_unclosed_call_is_not_wrapped() {
        let w = typed_word_at("bsha256(md5(", Some(1));
        assert_eq!(w.wrapped, "sha256", "only the name, not the open call");
        assert_eq!(w.wrap_end, w.end);
    }

    use super::*;

    fn parse(src: &str) -> Result<Expr, String> {
        Parser::new(src).parse_all()
    }

    /// A pinned clock and a pinned "random" source, so a signature can be
    /// checked against a known value. `fill_random` produces a fixed, repeating
    /// byte pattern; it is not random and is not meant to be.
    struct FakeSource {
        secs: i64,
        counters: std::sync::Mutex<HashMap<String, u64>>,
        request: Option<RequestFacts>,
    }

    impl FakeSource {
        fn at(secs: i64) -> Self {
            FakeSource {
                secs,
                counters: std::sync::Mutex::new(HashMap::new()),
                request: None,
            }
        }

        fn sending(mut self, request: RequestFacts) -> Self {
            self.request = Some(request);
            self
        }
    }

    impl GenSource for FakeSource {
        fn now(&self) -> (i64, u32) {
            (self.secs, 123_000_000)
        }
        fn fill_random(&self, buf: &mut [u8]) {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
        }
        fn request(&self) -> Option<&RequestFacts> {
            self.request.as_ref()
        }

        fn counter(&self, name: &str) -> u64 {
            let mut c = self.counters.lock().unwrap();
            let n = c.entry(name.to_string()).or_insert(0);
            *n += 1;
            *n
        }
    }

    fn run(rows: &[(&str, &str)]) -> (HashMap<String, String>, Vec<GenError>) {
        run_with(rows, HashMap::new())
    }

    /// The same walk, but with a request behind the block, which is what the
    /// runner always has and the editor's live check never does.
    fn run_sending(
        request: RequestFacts,
        rows: &[(&str, &str)],
    ) -> (HashMap<String, String>, Vec<GenError>) {
        let rows: Vec<(String, String)> = rows
            .iter()
            .map(|(n, e)| (n.to_string(), e.to_string()))
            .collect();
        let mut vars = HashMap::new();
        let errors = expand(
            &rows,
            &mut vars,
            &FakeSource::at(1_700_000_000).sending(request),
        );
        (vars, errors)
    }

    fn facts() -> RequestFacts {
        RequestFacts {
            method: "post".to_string(),
            url: "https://api.example.net/v2/orders?page=2&size=10#top".to_string(),
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Trace".to_string(), "first".to_string()),
                ("X-Trace".to_string(), "second".to_string()),
            ],
            body: r#"{"id":7}"#.to_string(),
            name: "Create order".to_string(),
        }
    }

    fn run_with(
        rows: &[(&str, &str)],
        mut vars: HashMap<String, String>,
    ) -> (HashMap<String, String>, Vec<GenError>) {
        let rows: Vec<(String, String)> = rows
            .iter()
            .map(|(n, e)| (n.to_string(), e.to_string()))
            .collect();
        let errors = expand(&rows, &mut vars, &FakeSource::at(1_700_000_000));
        (vars, errors)
    }

    /// Published test vectors, not values this implementation produced. A
    /// signature function that is self-consistently wrong is indistinguishable
    /// from a correct one until a server rejects it, so the only test worth
    /// having is one written against numbers from outside this codebase.
    ///
    /// Digests are of `"abc"` (FIPS 180-4 / RFC 1321 examples).
    #[test]
    fn the_digests_match_their_published_vectors() {
        let (v, e) = run(&[
            ("md5", r#"md5("abc")"#),
            ("sha1", r#"sha1("abc")"#),
            ("sha256", r#"sha256("abc")"#),
            ("sha512", r#"sha512("abc")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["md5"], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(v["sha1"], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            v["sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            v["sha512"],
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    /// RFC 2202 (HMAC-SHA1) and RFC 4231 (HMAC-SHA256/512) test case 2:
    /// key `"Jefe"`, data `"what do ya want for nothing?"`.
    #[test]
    fn the_macs_match_their_published_vectors() {
        let (v, e) = run(&[
            ("s1", r#"hmac_sha1("Jefe", "what do ya want for nothing?")"#),
            (
                "s256",
                r#"hmac_sha256("Jefe", "what do ya want for nothing?")"#,
            ),
            (
                "s512",
                r#"hmac_sha512("Jefe", "what do ya want for nothing?")"#,
            ),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["s1"], "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
        assert_eq!(
            v["s256"],
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            v["s512"],
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
             9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
    }

    /// `_b64` encodes the *digest bytes*, which is not the same as Base64 of the
    /// hex text — a distinction worth a test, because getting it wrong produces
    /// a plausible-looking string that every server rejects.
    #[test]
    fn the_b64_variants_encode_the_digest_not_its_hex() {
        let (v, e) = run(&[
            ("hex", r#"sha256("abc")"#),
            ("b64", r#"sha256_b64("abc")"#),
            ("wrong", r#"base64(sha256("abc"))"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        // Base64 of the SHA-256 digest of "abc" — i.e. of the published
        // ba7816bf… bytes, not of the 64 characters that spell them.
        assert_eq!(v["b64"], "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        assert_ne!(v["b64"], b64(v["hex"].as_bytes(), false));
        assert_eq!(
            v["wrong"],
            b64(v["hex"].as_bytes(), false),
            "base64() of a hash still encodes the hex text — the reason the \
             _b64 variants exist"
        );
    }

    /// The URL-safe alphabet is not cosmetic: `+`, `/` and `=` are all wrong
    /// in a JWT segment or a query parameter, which is the only reason these
    /// variants exist.
    #[test]
    fn the_url_safe_digests_use_the_alphabet_a_jwt_needs() {
        let (v, e) = run(&[
            ("padded", r#"sha256_b64("abc")"#),
            ("safe", r#"sha256_b64url("abc")"#),
            ("mac", r#"hmac_sha256_b64url("key", "message")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        // The same digest as the padded vector above, with `+` -> `-`,
        // `/` -> `_` and the padding dropped.
        assert_eq!(v["padded"], "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        assert_eq!(v["safe"], "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0");
        assert_eq!(
            v["mac"],
            encode_digest(
                &hmac_bytes("sha256", b"key", b"message"),
                DigestEncoding::B64Url
            )
        );
        for name in ["safe", "mac"] {
            assert!(
                !v[name].contains(['+', '/', '=']),
                "{name} produced {:?}, which cannot go in a URL or a JWT",
                v[name]
            );
        }
    }

    /// The pieces a script would reach for: the last segment of a path, a
    /// field out of a header, the token after a space.
    #[test]
    fn split_counts_from_either_end_and_refuses_to_guess() {
        let (v, e) = run(&[
            ("first", r#"split("a/b/c", "/", 0)"#),
            ("last", r#"split("a/b/c", "/", -1)"#),
            ("but_one", r#"split("a/b/c", "/", -2)"#),
            ("token", r#"split("Bearer abc123", " ", 1)"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["first"], "a");
        assert_eq!(v["last"], "c");
        assert_eq!(v["but_one"], "b");
        assert_eq!(v["token"], "abc123");

        // Out of range, an empty separator and a non-numeric index are all
        // faults rather than an empty answer: this text goes on to be signed
        // or sent, and quietly nothing is the hardest kind of wrong to find.
        for expr in [
            r#"split("a/b", "/", 5)"#,
            r#"split("a/b", "/", -5)"#,
            r#"split("a/b", "", 0)"#,
            r#"split("a/b", "/", "last")"#,
        ] {
            let (v, e) = run(&[("v", expr)]);
            assert!(
                matches!(e.as_slice(), [GenError::BadArgument { .. }]),
                "{expr} gave {e:?}"
            );
            assert!(!v.contains_key("v"), "{expr} still set a value");
        }
    }

    /// A JSON document written as a `[Gen]` string literal, quotes and all --
    /// the shape a row gets it in when it comes from `body()` or a capture.
    fn as_literal(json: &str) -> String {
        format!("\"{}\"", json.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// The shapes a ported script reaches for: a field, a nested field, an
    /// element, a key that cannot be written with a dot, and a whole
    /// sub-document to hash.
    #[test]
    fn jsonpath_reaches_into_a_document_the_block_already_has() {
        let doc = r#"{"a":{"b":"x"},"items":[{"id":7},{"id":8}],"odd key":"k",
                      "n":42,"ok":true,"sub":{"z":1}}"#;
        let (v, e) = run(&[
            ("doc", &as_literal(doc)),
            ("field", r#"jsonpath(doc, "$.a.b")"#),
            ("element", r#"jsonpath(doc, "$.items[1].id")"#),
            ("bracketed", r#"jsonpath(doc, "$['odd key']")"#),
            ("number", r#"jsonpath(doc, "$.n")"#),
            ("boolean", r#"jsonpath(doc, "$.ok")"#),
            ("whole", r#"jsonpath(doc, "$.sub")"#),
            ("root", r#"jsonpath(doc, "$")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["field"], "x");
        assert_eq!(v["element"], "8");
        assert_eq!(v["bracketed"], "k");
        // A number and a boolean come back as the text they are written as --
        // not quoted, not rounded.
        assert_eq!(v["number"], "42");
        assert_eq!(v["boolean"], "true");
        // A sub-document is compact JSON: the only sensible text for it, and
        // what a script hashing part of a payload wants.
        assert_eq!(v["whole"], r#"{"z":1}"#);
        assert!(v["root"].starts_with('{'));
    }

    /// The one filter shape the report columns needed, which a `[Gen]` row gets
    /// for free by sharing their walker: an API that returns its fields as a
    /// *list of key/value objects* has no addressable path to a named field
    /// without it.
    #[test]
    fn jsonpath_can_pick_an_element_out_of_a_list_by_one_of_its_fields() {
        let doc = r#"{"CardInfo":[{"key":"full_name","value":"Ada"},
                       {"key":"dob","value":"1815-12-10"}]}"#;
        let (v, e) = run(&[
            ("doc", &as_literal(doc)),
            (
                "name",
                r#"jsonpath(doc, "$.CardInfo[?(@.key=='full_name')].value")"#,
            ),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["name"], "Ada");
    }

    /// Everything that is not there is a fault, never an empty answer: the
    /// value goes on to be signed or sent, and quietly nothing is the hardest
    /// kind of wrong to find. `null` included -- "null" is four plausible
    /// characters that are never what was meant.
    #[test]
    fn jsonpath_refuses_rather_than_answering_with_nothing() {
        let doc = r#"{"a":{"b":"x"},"items":[1],"nothing":null}"#;
        let (v, e) = run(&[
            ("doc", &as_literal(doc)),
            ("missing", r#"jsonpath(doc, "$.a.nope")"#),
            ("past_end", r#"jsonpath(doc, "$.items[3]")"#),
            ("into_scalar", r#"jsonpath(doc, "$.a.b.c")"#),
            ("null_value", r#"jsonpath(doc, "$.nothing")"#),
            ("no_dollar", r#"jsonpath(doc, "a.b")"#),
            ("not_json", r#"jsonpath("<html>", "$.a")"#),
            ("bad_index", r#"jsonpath(doc, "$.items[x]")"#),
        ]);
        for name in [
            "missing",
            "past_end",
            "into_scalar",
            "null_value",
            "no_dollar",
            "not_json",
            "bad_index",
        ] {
            assert!(!v.contains_key(name), "{name} was given a value: {v:?}");
        }
        assert_eq!(e.len(), 7, "{e:?}");
        assert!(
            e.iter()
                .all(|err| matches!(err, GenError::BadArgument { .. })),
            "{e:?}"
        );
    }

    /// The paths the shared walker does not implement say so, and say where to
    /// go instead. Hurl's own JSONPath is a full implementation and its module
    /// is private, so anything unsupported here has to *fail* rather than be
    /// half-answered: the same path written in a `[Gen]` row and a `[Captures]`
    /// row quietly meaning different things is the worst outcome available.
    #[test]
    fn jsonpath_refuses_the_notation_it_does_not_share_with_hurl() {
        let doc = r#"{"items":[{"id":1},{"id":2}]}"#;
        for path in [
            "$..id",
            "$.items[*].id",
            "$.items[0:1]",
            "$.items[0,1]",
            "$.*",
        ] {
            let (v, e) = run(&[
                ("doc", &as_literal(doc)),
                ("v", &format!(r#"jsonpath(doc, "{path}")"#)),
            ]);
            assert!(!v.contains_key("v"), "{path} was answered");
            let detail = match e.as_slice() {
                [GenError::BadArgument { detail, .. }] => detail.clone(),
                other => panic!("{path} gave {other:?}"),
            };
            assert!(
                detail.contains("[Captures]"),
                "{path} should point at the row that can do it, said {detail:?}"
            );
        }
    }

    #[test]
    fn regex_answers_with_the_capture_group_when_the_pattern_names_one() {
        let (v, e) = run(&[
            ("whole", r#"regex("order-4711-x", "[0-9]+")"#),
            ("part", r#"regex("order-4711-x", "order-([0-9]+)")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["whole"], "4711");
        assert_eq!(v["part"], "4711");

        for expr in [
            r#"regex("nothing here", "[0-9]+")"#,
            r#"regex("text", "([")"#,
        ] {
            let (_, e) = run(&[("v", expr)]);
            assert!(
                matches!(e.as_slice(), [GenError::BadArgument { .. }]),
                "{expr} gave {e:?}"
            );
        }
    }

    /// A row may be a plain string: the common case for "this test case
    /// expects this" is data, not a computation, and making the user wrap it
    /// in `concat()` to satisfy the grammar would be a toll booth.
    #[test]
    fn a_row_may_be_a_plain_literal() {
        let (v, e) = run(&[("expected", r#""APPROVED""#), ("n", "3")]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["expected"], "APPROVED");
        assert_eq!(v["n"], "3");
    }

    /// The mistake every user of the rest of PaperBoy will make, because a URL,
    /// a header, a body and an assert all take `{{name}}`. Accepting it inside
    /// an expression would sign the braces themselves -- a wrong signature that
    /// looks right -- so it is a parse error, worded as the fix.
    #[test]
    fn a_placeholder_in_an_expression_is_refused_not_signed() {
        for expr in [
            "{{VAR}}",
            r#""{{VAR}}""#,
            r#"hmac_sha256("{{SECRET}}", "m")"#,
            r#"concat("x-", "{{VAR}}")"#,
        ] {
            let detail = parse(expr).expect_err(&format!("{expr} should not parse"));
            assert!(
                detail.contains("VAR") || detail.contains("SECRET"),
                "{expr} said {detail:?}, which does not name the variable"
            );
            assert!(
                detail.contains("not `{{"),
                "{expr} said {detail:?}, which does not say what to write instead"
            );
        }
        // Half a placeholder has no name to offer, so the message states the
        // rule rather than guessing at one.
        assert!(
            parse(r#""{{oops""#)
                .expect_err("unclosed braces")
                .contains("named directly"),
        );
    }

    /// The way out, for a string that really is meant to carry braces -- a body
    /// template being built for something else to fill in.
    #[test]
    fn an_escaped_brace_is_a_brace() {
        let (v, e) = run(&[("a", r#""\{{VAR}}""#)]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["a"], "{{VAR}}");
    }

    #[test]
    fn a_block_reads_the_request_it_belongs_to() {
        let (v, e) = run_sending(
            facts(),
            &[
                ("m", "method()"),
                ("u", "url()"),
                ("p", "path()"),
                ("q", "query()"),
                ("b", "body()"),
                ("n", "request_name()"),
                ("ct", r#"header("content-type")"#),
                ("trace", r#"header("X-Trace")"#),
                ("absent", r#"header("X-Nope")"#),
            ],
        );
        assert!(e.is_empty(), "{e:?}");
        // Upper-cased: what goes in a signing string is the method as sent,
        // not as typed.
        assert_eq!(v["m"], "POST");
        assert_eq!(
            v["u"],
            "https://api.example.net/v2/orders?page=2&size=10#top"
        );
        assert_eq!(v["p"], "/v2/orders");
        assert_eq!(v["q"], "page=2&size=10");
        assert_eq!(v["b"], r#"{"id":7}"#);
        assert_eq!(v["n"], "Create order");
        // Case-insensitive, first of a repeated name wins, and a header that
        // isn't there is empty rather than a fault.
        assert_eq!(v["ct"], "application/json");
        assert_eq!(v["trace"], "first");
        assert_eq!(v["absent"], "");
    }

    /// A URL that is still `{{base}}/x` has no host to strip, and half a URL
    /// is not a mistake in the block -- it is a URL whose front end arrives
    /// from the environment.
    #[test]
    fn a_templated_url_still_yields_the_part_that_is_written_down() {
        let mut r = facts();
        r.url = "{{base}}/v2/orders?page=2".to_string();
        let (v, e) = run_sending(r, &[("p", "path()"), ("q", "query()")]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["p"], "/v2/orders");
        assert_eq!(v["q"], "page=2");

        let mut bare = facts();
        bare.url = "{{base}}".to_string();
        let (v, e) = run_sending(bare, &[("p", "path()"), ("q", "query()")]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["p"], "");
        assert_eq!(v["q"], "");
    }

    /// The ordering rule, stated as a test: a row reading the request sees the
    /// rows above it substituted and the rows below it still as `{{name}}`.
    /// Anything else makes a value depend on a row that depends on it.
    #[test]
    fn a_row_reading_the_request_sees_the_rows_above_it_filled_in() {
        let mut r = facts();
        r.body = r#"{"first":"{{a}}","second":"{{z}}"}"#.to_string();
        let (v, e) = run_sending(
            r,
            &[
                ("a", r#"hex("41")"#),
                ("snapshot", "body()"),
                ("z", "hex(\"5A\")"),
            ],
        );
        assert!(e.is_empty(), "{e:?}");
        // `hex` encodes the text "41", i.e. "3431" -- the point is which
        // rows had run by the time `body()` was asked, not the value itself.
        assert_eq!(v["snapshot"], r#"{"first":"3431","second":"{{z}}"}"#);
    }

    /// Without a request -- the editor's live check on a block being typed --
    /// these say so rather than answering with nothing, because an HMAC over a
    /// silently empty body is a signature that authorises nothing.
    #[test]
    fn a_request_function_with_no_request_behind_it_says_so() {
        for expr in [
            "method()",
            "url()",
            "path()",
            "query()",
            "body()",
            "request_name()",
            r#"header("Accept")"#,
        ] {
            let (v, e) = run(&[("v", expr)]);
            assert!(
                matches!(e.as_slice(), [GenError::NoRequest { .. }]),
                "{expr} gave {e:?}"
            );
            assert!(!v.contains_key("v"), "{expr} still set a value");
        }
    }

    /// What the block reads is what Hurl will send: the wire body, without the
    /// JSON comments the editor keeps, and without header rows the user has
    /// switched off.
    #[test]
    fn the_request_a_block_reads_is_the_one_that_will_be_sent() {
        let entry = crate::hurl::HurlEntry {
            title: "Create order".to_string(),
            method: "POST".to_string(),
            url: "https://api.example.net/v2/orders".to_string(),
            headers: vec![
                crate::hurl::KvRow::new("Accept", "application/json"),
                crate::hurl::KvRow::toggled("X-Debug", "1", false),
            ],
            body_src: Some("{\n  // the id the server assigns\n  \"id\": 7\n}".to_string()),
            ..Default::default()
        };
        let f = RequestFacts::of(&entry);
        assert_eq!(f.name, "Create order");
        assert_eq!(f.method, "POST");
        assert_eq!(
            f.headers,
            vec![("Accept".to_string(), "application/json".to_string())],
            "a switched-off header is not sent, so it is not part of what is signed"
        );
        assert!(
            !f.body.contains("//"),
            "the block signed the editor's comments, not the bytes on the wire: {:?}",
            f.body
        );
        assert!(f.body.contains("\"id\""), "{:?}", f.body);
    }

    /// The shape real signing takes: a nonce and a timestamp computed here,
    /// then signed, with the signature reading the rows above it.
    #[test]
    fn a_block_can_sign_the_values_it_just_computed() {
        let mut vars = HashMap::new();
        vars.insert("SECRET".to_string(), "s3cr3t".to_string());
        let (v, e) = run_with(
            &[
                ("nonce", "random_hex(16)"),
                ("stamp", "timestamp"),
                (
                    "sig",
                    r#"hmac_sha256_b64(SECRET, concat(nonce, ":", stamp))"#,
                ),
            ],
            vars,
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["stamp"], "1700000000");
        let expected = encode_digest(
            &hmac_bytes(
                "sha256",
                b"s3cr3t",
                format!("{}:{}", v["nonce"], v["stamp"]).as_bytes(),
            ),
            DigestEncoding::B64,
        );
        assert_eq!(
            v["sig"], expected,
            "the signature covers both computed rows"
        );
    }

    /// Arity is checked for the signing functions too — a one-argument
    /// `hmac_sha256` is a missing key, which must not quietly sign with none.
    #[test]
    fn a_mac_with_the_wrong_number_of_arguments_is_refused() {
        let (v, e) = run(&[("sig", r#"hmac_sha256("only-a-key")"#)]);
        assert!(!v.contains_key("sig"), "nothing is bound");
        assert!(
            matches!(&e[..], [GenError::Arity { function, expected, got, .. }]
                if function == "hmac_sha256" && expected == "2" && *got == 1),
            "{e:?}"
        );
    }

    #[test]
    fn the_grammar_reads_calls_references_and_literals() {
        assert_eq!(parse("uuid"), Ok(Expr::Reference("uuid".into())));
        assert_eq!(
            parse("timestamp(-30)"),
            Ok(Expr::Call {
                function: "timestamp".into(),
                args: vec![Expr::Number("-30".into())]
            })
        );
        assert_eq!(
            parse(r#"concat("a", B)"#),
            Ok(Expr::Call {
                function: "concat".into(),
                args: vec![Expr::Text("a".into()), Expr::Reference("B".into())]
            })
        );
        // Nesting is the point: this is the shape real signing takes.
        assert_eq!(
            parse(r#"base64(hmac_sha256(K, concat("GET\n", P)))"#),
            Ok(Expr::Call {
                function: "base64".into(),
                args: vec![Expr::Call {
                    function: "hmac_sha256".into(),
                    args: vec![
                        Expr::Reference("K".into()),
                        Expr::Call {
                            function: "concat".into(),
                            args: vec![Expr::Text("GET\n".into()), Expr::Reference("P".into())]
                        }
                    ]
                }]
            })
        );
        assert_eq!(
            parse("uuid()"),
            Ok(Expr::Call {
                function: "uuid".into(),
                args: vec![]
            })
        );
    }

    /// Trailing text is refused rather than ignored. Silently dropping the tail
    /// of an expression is precisely the Hurl behaviour that made this whole
    /// feature necessary; repeating it here would be unforgivable.
    #[test]
    fn trailing_rubbish_is_an_error_not_something_to_ignore() {
        assert!(parse("uuid junk").is_err());
        assert!(parse("timestamp() extra").is_err());
        assert!(parse(r#"concat("a") "b""#).is_err());
    }

    #[test]
    fn malformed_expressions_are_rejected_with_a_reason() {
        for bad in [
            "",
            "   ",
            "concat(",
            "concat(a",
            r#"concat("a)"#,
            "concat(a b)",
            "-",
            r#""\q""#,
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_string_carries_the_escapes_a_signing_string_needs() {
        assert_eq!(parse(r#""a\nb""#), Ok(Expr::Text("a\nb".into())));
        assert_eq!(parse(r#""a\"b""#), Ok(Expr::Text("a\"b".into())));
        assert_eq!(parse(r#""a\\b""#), Ok(Expr::Text("a\\b".into())));
        // The characters that mark up the .hurl file are ordinary here.
        assert_eq!(parse(r#""a#b:c""#), Ok(Expr::Text("a#b:c".into())));
    }

    /// An example is offered as a thing to click, so it has to be a call that
    /// runs: one with a stale name, the wrong arity or a format string the
    /// formatter chokes on would put a broken expression into the user's
    /// request under the guise of help.
    #[test]
    fn every_offered_example_is_a_call_that_works() {
        for f in FUNCTIONS {
            for ex in f.examples {
                assert!(
                    ex.starts_with(f.name),
                    "{ex:?} is offered under {} but does not call it",
                    f.name
                );
                let (vars, errors) = run(&[("v", ex)]);
                assert!(errors.is_empty(), "{ex:?} did not run: {errors:?}");
                assert!(
                    !vars["v"].is_empty(),
                    "{ex:?} ran but produced nothing at all"
                );
                // A strftime typo is not an error to chrono -- an unknown
                // specifier is copied through verbatim -- so a percent sign
                // surviving into the output means the pattern was not
                // understood.
                assert!(
                    !vars["v"].contains('%'),
                    "{ex:?} produced {:?}, so part of the format was not understood",
                    vars["v"]
                );
            }
        }
    }

    #[test]
    fn time_functions_read_the_injected_clock() {
        let (v, e) = run(&[
            ("a", "timestamp"),
            ("b", "timestamp(-30)"),
            ("c", "timestamp_ms"),
            ("d", "iso8601"),
            ("f", r#"date("%Y-%m-%d")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["a"], "1700000000");
        assert_eq!(v["b"], "1699999970");
        assert_eq!(v["c"], "1700000000123");
        assert_eq!(v["d"], "2023-11-14T22:13:20Z");
        assert_eq!(v["f"], "2023-11-14");
    }

    #[test]
    fn encoding_functions_produce_the_expected_bytes() {
        let (v, e) = run(&[
            ("a", r#"base64("hello")"#),
            ("b", r#"base64_decode("aGVsbG8=")"#),
            ("c", r#"hex("AB")"#),
            ("d", r#"urlencode("a b&c=d")"#),
            ("f", r#"urldecode("a%20b%26c")"#),
            ("g", r#"json_string("a\"b")"#),
            ("h", r#"base64url("~~~")"#),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["a"], "aGVsbG8=");
        assert_eq!(v["b"], "hello");
        assert_eq!(v["c"], "4142");
        // `&` and `=` must be encoded: under-encoding changes what gets signed.
        assert_eq!(v["d"], "a%20b%26c%3Dd");
        assert_eq!(v["f"], "a b&c");
        assert_eq!(v["g"], r#""a\"b""#);
        // Standard base64 of "~~~" is `fn5+`; the URL-safe alphabet must not
        // emit `+`, `/` or padding, which a URL or a JWT header can't carry.
        assert_eq!(v["h"], "fn5-");
    }

    #[test]
    fn text_functions_build_a_canonical_string() {
        let (v, e) = run_with(
            &[(
                "s",
                r#"concat(upper(METHOD), "\n", lower(HOST), "\n", trim(P))"#,
            )],
            HashMap::from([
                ("METHOD".to_string(), "get".to_string()),
                ("HOST".to_string(), "API.Example.COM".to_string()),
                ("P".to_string(), "  /orders  ".to_string()),
            ]),
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["s"], "GET\napi.example.com\n/orders");
    }

    #[test]
    fn random_functions_respect_the_length_and_range_asked_for() {
        let (v, e) = run(&[
            ("a", "random_hex(32)"),
            ("b", "random_hex(7)"),
            ("c", "random_alnum(12)"),
            ("d", "random_base64(16)"),
            ("f", "random_int(5, 5)"),
            ("g", "counter(\"page\")"),
            ("h", "counter(\"page\")"),
        ]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["a"].len(), 32);
        // An odd length must not be rounded up to the whole byte behind it.
        assert_eq!(v["b"].len(), 7);
        assert!(v["a"].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(v["c"].len(), 12);
        assert!(v["c"].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(v["d"].len(), 24, "16 bytes is 24 base64 characters");
        assert_eq!(v["f"], "5");
        assert_eq!((&v["g"], &v["h"]), (&"1".to_string(), &"2".to_string()));
    }

    /// A row may build on the rows above it and on the environment, which is
    /// the entire point — a signature is a function of a nonce and a secret.
    #[test]
    fn a_row_can_build_on_earlier_rows_and_on_the_environment() {
        let (v, e) = run_with(
            &[
                ("nonce", "random_hex(8)"),
                ("stamp", "timestamp"),
                ("payload", r#"concat(nonce, ":", stamp, ":", TENANT)"#),
                ("encoded", "base64(payload)"),
            ],
            HashMap::from([("TENANT".to_string(), "acme".to_string())]),
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["payload"], format!("{}:1700000000:acme", v["nonce"]));
        assert_eq!(v["encoded"], b64(v["payload"].as_bytes(), false));
    }

    /// The same name used twice in one request is one value, so a nonce written
    /// into a header and into the body it signs matches. (Postman re-evaluates
    /// per use, which makes exactly that case impossible.)
    #[test]
    fn a_generator_is_evaluated_once_and_reused() {
        let (v, e) = run(&[("n", "random_hex(16)"), ("copy", "n"), ("again", "n")]);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["n"], v["copy"]);
        assert_eq!(v["n"], v["again"]);
    }

    /// A failed row binds nothing. Its `{{name}}` then stays unresolved and the
    /// run is refused, rather than a request going out signed with "".
    #[test]
    fn a_failed_row_binds_nothing_and_the_others_still_run() {
        let (v, e) = run(&[
            ("good", r#"upper("a")"#),
            ("bad", "no_such_function(1)"),
            ("after", r#"lower("B")"#),
        ]);
        assert_eq!(v.get("bad"), None, "a failed row must not bind a value");
        assert_eq!(v["good"], "A");
        assert_eq!(v["after"], "b", "a later row still runs");
        assert_eq!(
            e,
            vec![GenError::UnknownFunction {
                name: "bad".into(),
                function: "no_such_function".into()
            }]
        );
    }

    #[test]
    fn each_kind_of_mistake_names_the_row_it_is_in() {
        let (_, e) = run_with(
            &[
                ("a", "timestamp(1, 2)"),
                ("b", "random_hex(\"lots\")"),
                ("c", "MISSING"),
                ("d", "concat("),
            ],
            HashMap::new(),
        );
        assert_eq!(e.len(), 4, "{e:?}");
        assert_eq!(
            e.iter().map(GenError::row).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
        assert!(matches!(e[0], GenError::Arity { .. }), "{:?}", e[0]);
        assert!(matches!(e[1], GenError::BadArgument { .. }), "{:?}", e[1]);
        assert_eq!(
            e[2],
            GenError::UndefinedReference {
                name: "c".into(),
                reference: "MISSING".into()
            }
        );
        assert!(matches!(e[3], GenError::Syntax { .. }), "{:?}", e[3]);
    }

    /// A row referring to itself, or to one below it, is refused — even when an
    /// environment variable of that name exists. Falling back to the variable
    /// would make a mis-ordered block work, differently, and only sometimes.
    #[test]
    fn a_row_referring_to_itself_or_to_one_below_it_is_refused() {
        let (v, e) = run_with(
            &[("a", "concat(a)")],
            HashMap::from([("a".to_string(), "from-the-environment".to_string())]),
        );
        assert_eq!(e, vec![GenError::Cycle { name: "a".into() }]);
        assert_eq!(
            v["a"], "from-the-environment",
            "the environment's value is untouched, not overwritten"
        );

        let (_, e) = run(&[("first", "concat(second)"), ("second", "timestamp")]);
        assert_eq!(
            e,
            vec![GenError::Cycle {
                name: "first".into()
            }]
        );
    }

    /// A row reading one that failed says so. The earlier row is unbound,
    /// which from the inside looks exactly like a row that has not run yet --
    /// so a block with one typo used to report the typo *and* "b refers to
    /// itself, or to a row below it", which is not true and is the louder of
    /// the two claims.
    #[test]
    fn a_row_reading_a_failed_row_is_not_accused_of_a_cycle() {
        let (_, e) = run(&[("a", "no_such_function()"), ("b", "concat(a)")]);
        assert_eq!(
            e,
            vec![
                GenError::UnknownFunction {
                    name: "a".into(),
                    function: "no_such_function".into()
                },
                GenError::FailedDependency {
                    name: "b".into(),
                    reference: "a".into()
                }
            ],
            "one mistake, and its consequence named as one"
        );
    }

    /// The environment is still not a fallback for a row that failed: reading
    /// it would make a block with a typo in it send a *different* request
    /// rather than none, which is the whole reason references are resolved
    /// against the block first.
    #[test]
    fn a_failed_row_does_not_fall_back_to_the_environment() {
        let (v, e) = run_with(
            &[("a", "no_such_function()"), ("b", "concat(a)")],
            HashMap::from([("a".to_string(), "from-the-environment".to_string())]),
        );
        assert!(
            matches!(e[1], GenError::FailedDependency { .. }),
            "{:?}",
            e[1]
        );
        assert!(!v.contains_key("b"), "and nothing is bound for b");
    }

    /// A generator's value must never be re-read as a template. If a secret
    /// contains `{{`, expanding it again would either leak or corrupt it.
    #[test]
    fn a_generated_value_is_never_treated_as_a_template() {
        let (v, e) = run_with(
            &[("out", "concat(SECRET)")],
            HashMap::from([("SECRET".to_string(), "{{OTHER}}".to_string())]),
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["out"], "{{OTHER}}");
    }

    /// The table and the implementation must agree about how a function is
    /// called: an editor that offers `hmac_sha256(key, message)` while `call`
    /// wants three arguments teaches the user something false, and the lesson
    /// is only corrected by a failed request.
    #[test]
    fn every_function_is_called_the_way_the_table_says() {
        let src = FakeSource::at(1_700_000_000);
        let arg = |n: usize| vec!["1".to_string(); n];
        let is_arity = |e: &GenError| matches!(e, GenError::Arity { .. });
        for f in FUNCTIONS {
            assert!(
                f.signature.starts_with(f.name),
                "{}'s signature must name it: {}",
                f.name,
                f.signature
            );
            // The signature is not just a label: both editors write it into the
            // cell as the starting point for a call, so it has to *be* a call
            // that `call` accepts. `counter()` used to advertise zero arguments
            // while `min_args` was 1, so completing it wrote a bare `counter`
            // that the next `check` rejected. Count the mandatory arguments the
            // signature spells (the ones outside `[optional]` brackets) and hold
            // them to `min_args`.
            let inside = f
                .signature
                .split_once('(')
                .and_then(|(_, rest)| rest.strip_suffix(')'))
                .unwrap_or("");
            let written = inside.split(',').filter(|a| !a.trim().is_empty()).count();
            let optional = inside.matches('[').count();
            assert!(
                written.saturating_sub(optional) >= f.min_args,
                "{}: the editors write `{}`, which `call` refuses — it wants {} argument(s)",
                f.name,
                f.signature,
                f.min_args
            );
            if f.min_args > 0 {
                let e = call(f.name, &arg(f.min_args - 1), "row", &HashMap::new(), &src)
                    .expect_err(&format!("{} accepted too few arguments", f.name));
                assert!(is_arity(&e), "{}: {e:?}", f.name);
            }
            if let Some(max) = f.max_args {
                let e = call(f.name, &arg(max + 1), "row", &HashMap::new(), &src)
                    .expect_err(&format!("{} accepted too many arguments", f.name));
                assert!(is_arity(&e), "{}: {e:?}", f.name);
            }
            // The right number may still be the wrong *value* — `date("1")` is
            // a format string that formats nothing — so only arity is asserted.
            if let Err(e) = call(f.name, &arg(f.min_args), "row", &HashMap::new(), &src) {
                assert!(!is_arity(&e), "{} rejected its own arity: {e:?}", f.name);
            }
        }
    }

    /// What an editor can say before anything is sent, and what it must not:
    /// a name that does not exist is always wrong, a variable it cannot see
    /// is not.
    #[test]
    fn checking_a_block_finds_typos_but_not_missing_variables() {
        let rows: Vec<(String, String)> = [
            ("a", "hmac_sha526(k, m)"),
            ("b", "random_int(1)"),
            ("c", "sha256("),
            ("d", "hmac_sha256(api_key, nothing_defines_this)"),
            ("e", "uuid"),
            ("", ""),
        ]
        .iter()
        .map(|(n, x)| (n.to_string(), x.to_string()))
        .collect();
        let found = check(&rows);
        let named: Vec<&str> = found.iter().map(|e| e.row()).collect();
        assert_eq!(
            named,
            vec!["a", "b", "c"],
            "an unknown function, a wrong arity and a syntax error — no more: {found:?}"
        );
    }

    /// A wholly blank row is the same to [`check`] (the editor) and to
    /// [`expand`] (the send): a row still being typed, ignored by both. When
    /// they disagreed, the editor showed no fault while the send was refused by
    /// a message that named no row.
    #[test]
    fn check_and_expand_agree_about_a_blank_row() {
        let rows = vec![(String::new(), String::new())];
        let found = check(&rows);
        let mut vars = HashMap::new();
        let raised = expand(&rows, &mut vars, &FakeSource::at(1_700_000_000));
        assert_eq!(
            found.len(),
            raised.len(),
            "check(): {found:?}\nexpand(): {raised:?}"
        );
        assert!(found.is_empty() && raised.is_empty());
    }

    /// The name is how a generated value reaches the request. A row without one
    /// used to evaluate happily into a variable no `{{placeholder}}` could ever
    /// name -- work done for nobody, reported as nothing.
    #[test]
    fn a_row_with_no_name_is_refused_by_both() {
        let rows = vec![(String::new(), "uuid".to_string())];
        let found = check(&rows);
        assert_eq!(found, vec![GenError::NameMissing], "{found:?}");
        let mut vars = HashMap::new();
        let raised = expand(&rows, &mut vars, &FakeSource::at(0));
        assert_eq!(found, raised, "the editor and the send say the same thing");
        assert!(
            vars.is_empty(),
            "and nothing is computed under an empty key: {vars:?}"
        );
        let english = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
        // The one message that cannot name its row, because the missing name is
        // the fault -- so it has to describe the row instead.
        assert!(
            crate::i18n::describe_gen_errors(&english, &found)[0].starts_with("A generated row"),
            "{found:?}"
        );
    }

    /// `{{my name}}` is not a placeholder Hurl will carry, so a row called
    /// `my name` can never be read -- the same rule the extract-to-parameter
    /// prompt already applies to the names it accepts.
    #[test]
    fn a_name_hurl_cannot_carry_is_refused() {
        let rows = vec![("my name".to_string(), "uuid".to_string())];
        let found = check(&rows);
        assert!(
            matches!(found.as_slice(), [GenError::NameInvalid { name }] if name == "my name"),
            "{found:?}"
        );
        let mut vars = HashMap::new();
        assert_eq!(expand(&rows, &mut vars, &FakeSource::at(0)), found);
        assert!(vars.is_empty(), "{vars:?}");
        // A name Hurl *will* carry is left alone, accents and all.
        let fine = vec![("kunde-id_2".to_string(), "uuid".to_string())];
        assert!(check(&fine).is_empty(), "{:?}", check(&fine));
    }

    /// Two rows with one name used to both evaluate, the later quietly
    /// overwriting the earlier: which value the request sent depended on the
    /// order of two rows that looked independent of each other.
    #[test]
    fn one_name_on_two_rows_is_reported_once() {
        let rows: Vec<(String, String)> = [
            ("token", "\"first\""),
            ("other", "uuid"),
            ("token", "\"second\""),
            ("token", "\"third\""),
        ]
        .iter()
        .map(|(n, x)| (n.to_string(), x.to_string()))
        .collect();
        let found = check(&rows);
        assert!(
            matches!(found.as_slice(), [GenError::NameDuplicate { name }] if name == "token"),
            "one sentence about one name, however many copies there are: {found:?}"
        );
        let mut vars = HashMap::new();
        let raised = expand(&rows, &mut vars, &FakeSource::at(0));
        assert_eq!(raised, found);
        assert_eq!(
            vars.get("token").map(String::as_str),
            Some("first"),
            "the row that read first is the one that stands: {vars:?}"
        );
        assert!(
            vars.contains_key("other"),
            "and the rows around it still evaluate: {vars:?}"
        );
    }

    /// A row that has been named but not filled in is half-written, not
    /// mistyped. It used to be reported as a parse failure -- "can't read the
    /// expression (expression is empty)" -- which said the same thing twice and
    /// sounded like the editor had broken.
    #[test]
    fn a_named_row_with_no_expression_says_what_is_missing() {
        let rows = vec![("token".to_string(), "  ".to_string())];
        let found = check(&rows);
        assert!(
            matches!(found.as_slice(), [GenError::Empty { name }] if name == "token"),
            "{found:?}"
        );
        // The send says the same thing about the same block.
        let mut vars = HashMap::new();
        let raised = expand(&rows, &mut vars, &FakeSource::at(0));
        assert_eq!(found, raised);
        let english = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
        assert_eq!(
            crate::i18n::describe_gen_errors(&english, &found),
            vec!["token: needs an expression".to_string()]
        );
    }

    /// Parsing, checking and evaluating are mutually recursive over nesting
    /// depth. A long enough line must be a normal error, not the stack overflow
    /// (a `SIGABRT` nothing catches) it used to be — [`check`] runs on every
    /// keystroke, so a crafted or corrupted collection could otherwise kill the
    /// app outright.
    #[test]
    fn deep_nesting_is_an_error_not_an_abort() {
        let expr = format!("{}\"x\"{}", "concat(".repeat(4000), ")".repeat(4000));
        let rows = vec![("a".to_string(), expr)];
        let found = check(&rows);
        assert!(
            matches!(found.first(), Some(GenError::Syntax { .. })),
            "deep nesting must be a syntax error: {found:?}"
        );
        // And the same on the send path, which shares the parser.
        let mut vars = HashMap::new();
        let raised = expand(&rows, &mut vars, &FakeSource::at(0));
        assert!(matches!(raised.first(), Some(GenError::Syntax { .. })));
    }

    /// A `timestamp` offset that doesn't fit in the clock is a `BadArgument`,
    /// like every other numeric argument — not a debug panic / release
    /// wraparound. The panic used to land on the send thread, which then never
    /// cleared `loading` and left the UI spinning.
    #[test]
    fn a_huge_timestamp_offset_is_an_error_not_a_panic() {
        let rows = vec![("t".to_string(), format!("timestamp({})", i64::MAX))];
        let mut vars = HashMap::new();
        let errors = expand(&rows, &mut vars, &FakeSource::at(1_700_000_000));
        assert!(
            matches!(errors.first(), Some(GenError::BadArgument { .. })),
            "{errors:?}"
        );
    }

    /// Error text reaches the status bar, the CLI's stderr and CI logs, so it
    /// must never quote a resolved argument: the arguments this feature handles
    /// include secrets.
    #[test]
    fn an_error_message_never_quotes_a_secrets_value() {
        let rows = vec![("n".to_string(), "random_hex(API_SECRET)".to_string())];
        let mut vars = HashMap::new();
        vars.insert("API_SECRET".to_string(), "hunter2-the-real-key".to_string());
        let errors = expand(&rows, &mut vars, &FakeSource::at(0));
        let english = crate::i18n::Strings::for_language(&crate::i18n::Language::English);
        let said = crate::i18n::describe_gen_errors(&english, &errors).join("; ");
        assert!(
            !said.contains("hunter2-the-real-key"),
            "the message quotes the secret it was given: {said}"
        );
    }

    /// The signature both editors write into a cell has to be a call that
    /// works: `counter()` advertised zero arguments while `call` wanted one.
    #[test]
    fn every_signature_in_the_table_is_a_call_that_works() {
        for f in FUNCTIONS {
            let inside = f
                .signature
                .split_once('(')
                .and_then(|(_, rest)| rest.strip_suffix(')'))
                .unwrap_or("");
            let written = inside.split(',').filter(|a| !a.trim().is_empty()).count();
            let optional = inside.matches('[').count();
            assert!(
                written.saturating_sub(optional) >= f.min_args,
                "{}: the editors write `{}`, which `call` refuses — it wants {} argument(s)",
                f.name,
                f.signature,
                f.min_args
            );
        }
    }
}
