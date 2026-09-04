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
pub trait GenSource {
    /// Now, as a Unix timestamp in seconds, and the nanosecond part.
    fn now(&self) -> (i64, u32);
    /// Fill `buf` with cryptographically-unpredictable bytes.
    fn fill_random(&self, buf: &mut [u8]);
    /// The next value of the named counter, starting at 1.
    fn counter(&self, name: &str) -> u64;
}

/// Why a generator couldn't be evaluated. Every variant names the row, because
/// a request may declare several and "one of them is wrong" is not a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenError {
    /// The expression didn't parse. Carries the offending fragment.
    Syntax { name: String, detail: String },
    /// No such function.
    UnknownFunction { name: String, function: String },
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
            GenError::Syntax { name, .. }
            | GenError::UnknownFunction { name, .. }
            | GenError::Arity { name, .. }
            | GenError::BadArgument { name, .. }
            | GenError::UndefinedReference { name, .. }
            | GenError::Cycle { name } => name,
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
        let expr = self.expr()?;
        self.skip_space();
        if !self.rest.is_empty() {
            return Err(format!("unexpected `{}`", self.rest.trim()));
        }
        Ok(expr)
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.skip_space();
        match self.rest.chars().next() {
            None => Err("expression is empty".to_string()),
            Some('"') => self.string(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) if is_name_char(c) => self.ident_or_call(),
            Some(c) => Err(format!("unexpected `{c}`")),
        }
    }

    fn string(&mut self) -> Result<Expr, String> {
        let mut out = String::new();
        let mut chars = self.rest.char_indices();
        chars.next(); // the opening quote
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    self.rest = &self.rest[i + 1..];
                    return Ok(Expr::Text(out));
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, other)) => return Err(format!("unknown escape `\\{other}`")),
                    None => return Err("string ends in a backslash".to_string()),
                },
                _ => out.push(c),
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

    fn ident_or_call(&mut self) -> Result<Expr, String> {
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
            args.push(self.expr()?);
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

/// The real clock, the real random source, and per-run counters.
#[derive(Default)]
pub struct SystemSource {
    counters: std::sync::Mutex<HashMap<String, u64>>,
}

impl SystemSource {
    pub fn new() -> Self {
        Self::default()
    }
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

    fn counter(&self, name: &str) -> u64 {
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let next = counters.entry(name.to_string()).or_insert(0);
        *next += 1;
        *next
    }
}

// ── Evaluation ──────────────────────────────────────────────────────────

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
    let mut errors = Vec::new();
    let mut done: Vec<&str> = Vec::new();

    for (name, source) in rows {
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
        match eval(&expr, name, vars, &declared, &done, src) {
            Ok(value) => {
                vars.insert(name.clone(), value);
                done.push(name.as_str());
            }
            Err(e) => errors.push(e),
        }
    }
    errors
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
            if FUNCTIONS.contains(&name.as_str()) {
                return call(name, &[], row, src);
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
            if !FUNCTIONS.contains(&function.as_str()) {
                return Err(GenError::UnknownFunction {
                    name: row.to_string(),
                    function: function.clone(),
                });
            }
            let mut values = Vec::with_capacity(args.len());
            for a in args {
                values.push(eval(a, row, vars, declared, done, src)?);
            }
            call(function, &values, row, src)
        }
    }
}

/// Apply a generator function to its already-evaluated arguments.
fn call(
    function: &str,
    args: &[String],
    row: &str,
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
    let count = |s: &String| -> Result<usize, GenError> {
        s.parse::<usize>()
            .map_err(|_| bad(format!("`{s}` is not a whole number")))
    };

    match function {
        // ── Time ────────────────────────────────────────────────────────
        "timestamp" => {
            arity("0 or 1", args.len() <= 1)?;
            let offset = match args.first() {
                None => 0,
                Some(a) => a
                    .parse::<i64>()
                    .map_err(|_| bad(format!("`{a}` is not a whole number of seconds")))?,
            };
            Ok((src.now().0 + offset).to_string())
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
                .map_err(|_| bad(format!("`{}` is not a whole number", args[0])))?;
            let hi = args[1]
                .parse::<i64>()
                .map_err(|_| bad(format!("`{}` is not a whole number", args[1])))?;
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

        _ => Err(GenError::UnknownFunction {
            name: row.to_string(),
            function: function.to_string(),
        }),
    }
}

/// Every function name, for the wizard's suggestions and for documentation.
/// Kept beside [`call`] so a function added there is offered here.
pub const FUNCTIONS: &[&str] = &[
    "timestamp",
    "timestamp_ms",
    "iso8601",
    "date",
    "uuid",
    "counter",
    "random_int",
    "random_hex",
    "random_alnum",
    "random_base64",
    "base64",
    "base64url",
    "base64_decode",
    "hex",
    "urlencode",
    "urldecode",
    "json_string",
    "concat",
    "upper",
    "lower",
    "trim",
];

fn utc(src: &dyn GenSource) -> chrono::DateTime<chrono::Utc> {
    let (secs, nanos) = src.now();
    chrono::DateTime::from_timestamp(secs, nanos).unwrap_or_default()
}

fn b64(bytes: &[u8], url_safe: bool) -> String {
    use base64::Engine;
    if url_safe {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    } else {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
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
    }

    impl FakeSource {
        fn at(secs: i64) -> Self {
            FakeSource {
                secs,
                counters: std::sync::Mutex::new(HashMap::new()),
            }
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
}
