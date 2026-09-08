//! Turning a response you have already received into `[Asserts]` and
//! `[Captures]` rows.
//!
//! The workflow this exists for is the one every API client offers and PaperBoy
//! did not: send a request once, look at what came back, point at a value and
//! say "check this next time" or "keep this for the next request". Without it
//! the only way to write an assert is to read the jsonpath off the screen and
//! type it back in by hand, which is both tedious and the easiest possible
//! place to make a typo that turns into an assert failing for a reason having
//! nothing to do with the server.
//!
//! Everything here is front-end agnostic — no `ratatui`, no `egui`, no
//! [`crate::i18n`]. It answers two questions and emits Hurl for the answer:
//!
//! * *what could I assert on?* — [`probes`] enumerates every subject a reply
//!   offers (status, duration, each header, every value in a JSON body), for a
//!   browsable list.
//! * *what is under my cursor?* — [`probe_at`] maps a byte offset in the raw
//!   body back to the innermost JSON value covering it, for a click or a
//!   selection.
//!
//! The [`Subject`]/[`Predicate`] vocabulary and [`assert_line`] are shared with
//! the Postman importer ([`crate::postman`]), which reduces `pm.expect(...)`
//! chains to the same pair. One emitter means an imported assert and a
//! hand-built one are spelled identically, and there is a single place where
//! Hurl's assert grammar is known.

use crate::i18n::Strings;
use serde_json::Value;
use std::ops::Range;

/// What an assertion (or a capture) is talking about — the Hurl *query* half of
/// an `[Asserts]` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// The response status code. Not an assert line: PaperBoy records the
    /// expected status on the `HTTP <status>` line
    /// ([`crate::hurl::HurlEntry::expected_status`]), which is where Hurl's own
    /// grammar puts it, so [`assert_line`] declines it.
    Status,
    /// Wall-clock time of the exchange, in milliseconds.
    Duration,
    /// A response header, by name.
    Header(String),
    /// A jsonpath into the response body.
    Json(String),
    /// The same, asserted on its element count rather than its value.
    JsonCount(String),
    /// The response body as text — the fallback for a reply that is not JSON,
    /// where "does it mention this?" is the only question we can ask.
    Body,
}

impl Subject {
    /// The Hurl query this subject spells: the part of an `[Asserts]` line
    /// before the predicate, and the whole of a `[Captures]` expression.
    pub fn query(&self) -> String {
        match self {
            // `status` is a query in its own right in Hurl's assert grammar,
            // even though PaperBoy prefers the `HTTP <status>` line for it.
            Subject::Status => "status".to_string(),
            Subject::Duration => "duration".to_string(),
            Subject::Header(name) => format!("header \"{}\"", escape_hurl(name)),
            // The path is already escaped for the *jsonpath* grammar by
            // `push_key`, but it is about to be written inside a Hurl
            // double-quoted string, which has escapes of its own. Both layers
            // have to be satisfied or a key holding a quote ends the string
            // early (the file stops parsing) and a key holding a backslash
            // arrives at the evaluator as some other character entirely.
            Subject::Json(path) => format!("jsonpath \"{}\"", escape_hurl(path)),
            Subject::JsonCount(path) => format!("jsonpath \"{}\" count", escape_hurl(path)),
            Subject::Body => "body".to_string(),
        }
    }

    /// Whether this subject is a number, and so only answers numeric
    /// predicates. `contains` on a count is nonsense that Hurl accepts the
    /// shape of and then fails at run time.
    fn numeric(&self) -> bool {
        matches!(
            self,
            Subject::Duration | Subject::JsonCount(_) | Subject::Status
        )
    }
}

/// The test half of an `[Asserts]` line.
///
/// The payload of the comparing variants is an *already formatted* Hurl
/// literal — `"abc"` with its quotes, `42`, `true` — not a raw value. The
/// Postman importer lifts those straight out of JavaScript source, where they
/// are already written as literals; [`literal`] produces the same thing from a
/// [`Value`] read out of a real response. Both feed one emitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    Eq(String),
    Ne(String),
    Gt(String),
    Lt(String),
    Contains(String),
    StartsWith(String),
    /// The query matches something at all, whatever its value.
    Exists,
    /// `.to.be.empty` / `.is.not.empty` — the bool is whether emptiness is what
    /// is expected.
    Empty(bool),
}

/// The Hurl `[Asserts]` line a subject and predicate spell, or `None` for a
/// pairing Hurl has no query or predicate for.
pub fn assert_line(subject: Subject, predicate: Predicate) -> Option<String> {
    // The status has a line of its own above `[Asserts]`; callers set
    // `expected_status` instead. Emitting both would check the same thing
    // twice and let the two disagree.
    if matches!(subject, Subject::Status) {
        return None;
    }
    let query = subject.query();
    let numeric = subject.numeric();
    let line = match predicate {
        Predicate::Eq(v) => format!("{query} == {v}"),
        Predicate::Ne(v) => format!("{query} != {v}"),
        Predicate::Gt(v) => format!("{query} > {v}"),
        Predicate::Lt(v) => format!("{query} < {v}"),
        Predicate::Contains(v) if !numeric => format!("{query} contains {v}"),
        Predicate::StartsWith(v) if !numeric => format!("{query} startsWith {v}"),
        Predicate::Exists => format!("{query} exists"),
        Predicate::Empty(true) if !numeric => format!("{query} isEmpty"),
        Predicate::Empty(false) if !numeric => format!("{query} not isEmpty"),
        _ => return None,
    };
    match subject {
        // A bare `duration` assert only makes sense as a time bound: `duration
        // exists` is true of every response ever received.
        Subject::Duration if !matches!(line.split(' ').nth(1), Some("<" | ">" | "==")) => None,
        _ => Some(line),
    }
}

/// The `[Captures]` row (name, query) that stores this subject in a variable.
pub fn capture_row(subject: &Subject, name: &str) -> (String, String) {
    (name.to_string(), subject.query())
}

/// Append a jsonpath key: a plain identifier as `.name`, anything else bracket-
/// quoted (`['a-b']`) so the path stays valid.
pub fn push_key(path: &mut String, key: &str) {
    let simple = !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_alphanumeric() || c == '_');
    if simple {
        path.push('.');
        path.push_str(key);
    } else {
        // A key containing a quote would end the bracket-quoted string early
        // and produce a path that parses as something else entirely; the
        // backslash escape is what jsonpath implementations accept.
        let escaped = key.replace('\\', "\\\\").replace('\'', "\\'");
        path.push_str(&format!("['{escaped}']"));
    }
}

/// Escape a string for a Hurl double-quoted literal.
///
/// Hurl's quoted strings take the JSON escapes; a raw newline or an unescaped
/// quote in the middle of one ends the line early and turns the rest of the
/// value into a parse error, so every value read out of a response goes through
/// here before it is written into a file.
fn escape_hurl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // A Hurl quoted string is a template: `{{name}}` in it is read as
            // a placeholder and compared against the *variable* `name`, and an
            // unbalanced `{{` stops the whole file parsing. Response bodies
            // carry such text routinely (any CMS or notification API), so the
            // brace is escaped back into a plain character. Hurl's templatiser
            // looks at the source spelling of each character, so an escaped
            // brace can never start a placeholder.
            '{' => out.push_str("\\u{007b}"),
            // Anything else non-printable would be invisible in the file and
            // indistinguishable from the character next to it when read back.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A JSON value written as the Hurl literal that compares equal to it, or
/// `None` for the containers — Hurl compares objects and arrays only through
/// queries into them, so "this array equals that array" has no spelling and a
/// caller must offer `count` or `exists` instead.
pub fn literal(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("null".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => plain_number(n),
        Value::String(s) => Some(format!("\"{}\"", escape_hurl(s))),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// A JSON number written the only way Hurl can read it: plain decimal.
///
/// Hurl's number grammar has no exponent, but `JSON.stringify` and Python's
/// `json.dumps` both emit one for small and large floats, so a reply carrying
/// `1.5e-3` would otherwise produce an assert line that stops the *whole
/// collection file* parsing — every request in it, not just this assert.
/// Serde's own rendering is used where it is already plain, and expanded here
/// where it is not. A number too big to write out in full declines instead:
/// the palette still offers `exists`, which is honest, where a rounded
/// comparison would quietly assert something the response never said.
fn plain_number(n: &serde_json::Number) -> Option<String> {
    let s = n.to_string();
    let Some((mantissa, exponent)) = s.split_once(['e', 'E']) else {
        return Some(s);
    };
    let exponent: i32 = exponent.parse().ok()?;
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    // Where the point sits once the exponent has been applied, counted in
    // digits from the left. It can land outside the digits at either end,
    // which is what the padding below is for.
    let point = whole.len() as i32 + exponent;
    // Refuse anything that would need more than a line's worth of zeroes.
    // `1e300` is a number no assertion is meaningfully written against.
    if point.abs() > 40 {
        return None;
    }
    let mut out = if point <= 0 {
        format!("0.{}{}", "0".repeat(-point as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{}{}", digits, "0".repeat(point as usize - digits.len()))
    } else {
        let (l, r) = digits.split_at(point as usize);
        format!("{l}.{r}")
    };
    if out.contains('.') {
        out = out.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    Some(format!("{sign}{out}"))
}

/// What the second step can do with the chosen subject.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Verb {
    /// Append this line to `[Asserts]`.
    Assert(Predicate),
    /// Set the entry's expected status, which lives on the `HTTP <status>`
    /// line rather than in `[Asserts]` — Hurl's own placement for it.
    ExpectStatus(u16),
    /// Add a `[Captures]` row, after asking for the variable name.
    Capture,
}

/// One assertable thing in a response, with the value that actually came back.
///
/// The value is what makes the "snapshot this reply as a test" case a single
/// keystroke: the offered predicate is pre-filled with what the server just
/// said, rather than leaving the user to copy it across by hand.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub subject: Subject,
    /// The observed value, or `None` when there isn't one to show (a non-JSON
    /// body, or a response that never arrived).
    pub value: Option<Value>,
}

/// How many probes a single response may contribute.
///
/// A JSON body is arbitrary in size and a paginated endpoint returning ten
/// thousand rows would otherwise build a list nobody can scroll and a string
/// allocation for every leaf in it. The cap is on the browsable list only —
/// [`probe_at`] still resolves any offset in a body of any size, so pointing at
/// a value keeps working past the limit.
pub const MAX_PROBES: usize = 5_000;

/// Guard against a body nested deeply enough to overflow the stack while
/// walking it. Hostile input aside, nothing legitimate is 128 deep.
const MAX_DEPTH: usize = 128;

/// Every subject a response offers, in the order they should be shown: status,
/// duration, headers, then the body depth-first in document order.
///
/// `preserve_order` is enabled for `serde_json` (see `Cargo.toml`), so object
/// keys come out in the order the server sent them and the list reads in the
/// same order as the body on screen.
pub fn probes(
    status: u16,
    duration_ms: Option<u64>,
    headers: &[(String, String)],
    body: &str,
) -> Vec<Probe> {
    let mut out = Vec::new();
    if status != 0 {
        out.push(Probe {
            subject: Subject::Status,
            value: Some(Value::from(status)),
        });
    }
    if let Some(ms) = duration_ms {
        out.push(Probe {
            subject: Subject::Duration,
            value: Some(Value::from(ms)),
        });
    }
    for (name, value) in headers {
        out.push(Probe {
            subject: Subject::Header(name.clone()),
            value: Some(Value::String(value.clone())),
        });
    }
    match serde_json::from_str::<Value>(body) {
        Ok(v) => walk(&v, &mut String::from("$"), 0, &mut out),
        // Not JSON (HTML, XML, plain text, or a truncated body): the text as a
        // whole is still something to ask "does it mention this?" about.
        Err(_) if !body.trim().is_empty() => out.push(Probe {
            subject: Subject::Body,
            value: None,
        }),
        Err(_) => {}
    }
    out
}

/// Depth-first walk emitting a probe per value.
///
/// Containers get one too, not just their leaves: an array's is a `count` (the
/// useful assert on a list is how long it is) and an object's is the object
/// itself, which answers `exists` and `not isEmpty`.
fn walk(v: &Value, path: &mut String, depth: usize, out: &mut Vec<Probe>) {
    if out.len() >= MAX_PROBES || depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::Array(items) => {
            out.push(Probe {
                subject: Subject::JsonCount(path.clone()),
                value: Some(Value::from(items.len())),
            });
            for (i, item) in items.iter().enumerate() {
                let mark = path.len();
                path.push_str(&format!("[{i}]"));
                walk(item, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        Value::Object(map) => {
            out.push(Probe {
                subject: Subject::Json(path.clone()),
                value: Some(v.clone()),
            });
            for (k, item) in map {
                let mark = path.len();
                push_key(path, k);
                walk(item, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        scalar => out.push(Probe {
            subject: Subject::Json(path.clone()),
            value: Some(scalar.clone()),
        }),
    }
}

/// Every assert needed to say "this part of the body deep-equals `value`".
///
/// Hurl has no predicate that takes an object, so a deep equality has to be
/// spelled out one leaf at a time — which is also what makes a failure
/// readable: the report names the field that differed rather than printing two
/// documents side by side. An array additionally gets its length pinned, since
/// per-index asserts alone would pass on a longer list that happens to start
/// the same way.
///
/// Paths are built here rather than by the caller so the spelling
/// (`$.a.b[0]`, and the bracket form for a key that needs it) stays identical
/// to the one [`probes`] offers.
pub fn deep_equality(base: &str, value: &Value) -> Vec<(Subject, Predicate)> {
    let mut out = Vec::new();
    let mut path = base.to_string();
    deep_walk(value, &mut path, 0, &mut out);
    out
}

fn deep_walk(v: &Value, path: &mut String, depth: usize, out: &mut Vec<(Subject, Predicate)>) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::Array(items) => {
            out.push((
                Subject::JsonCount(path.clone()),
                Predicate::Eq(items.len().to_string()),
            ));
            for (i, item) in items.iter().enumerate() {
                let mark = path.len();
                path.push_str(&format!("[{i}]"));
                deep_walk(item, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                let mark = path.len();
                push_key(path, k);
                deep_walk(item, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        scalar => {
            if let Some(lit) = literal(scalar) {
                out.push((Subject::Json(path.clone()), Predicate::Eq(lit)));
            }
        }
    }
}

/// The predicates worth offering for a probe, best first.
///
/// "Best" is the equality against the value that just came back: the common
/// case is pinning down a reply that is already correct. The looser tests
/// follow for the fields that legitimately change between runs (an id, a
/// timestamp, a token) where only the shape can be asserted.
pub fn default_predicates(probe: &Probe) -> Vec<Predicate> {
    let mut out = Vec::new();
    let value = probe.value.as_ref();
    match &probe.subject {
        Subject::Status => {
            if let Some(v) = value.and_then(literal) {
                out.push(Predicate::Eq(v));
            }
        }
        Subject::Duration => {
            // A bound with headroom, not the exact time observed: the same
            // request is never the same number of milliseconds twice, so
            // `== 214` is an assert that fails on the next run by design.
            let ms = value.and_then(|v| v.as_u64()).unwrap_or(0);
            let budget = (ms.max(100) * 2).div_ceil(100) * 100;
            out.push(Predicate::Lt(budget.to_string()));
        }
        Subject::Header(_) | Subject::Json(_) => {
            let container = matches!(value, Some(Value::Object(_) | Value::Array(_)));
            if let Some(v) = value.and_then(literal) {
                out.push(Predicate::Eq(v.clone()));
                out.push(Predicate::Ne(v.clone()));
                // `contains`/`startsWith` read as text tests; offering them for
                // a number or a bool invites an assert that can never pass.
                if matches!(value, Some(Value::String(_))) {
                    out.push(Predicate::Contains(v.clone()));
                    out.push(Predicate::StartsWith(v));
                }
            }
            out.push(Predicate::Exists);
            if container || matches!(value, Some(Value::String(_)) | None) {
                out.push(Predicate::Empty(false));
                out.push(Predicate::Empty(true));
            }
        }
        Subject::JsonCount(_) => {
            if let Some(v) = value.and_then(literal) {
                out.push(Predicate::Eq(v));
            }
            out.push(Predicate::Gt("0".to_string()));
        }
        Subject::Body => {
            // There is no observed value to pre-fill from — the whole body is
            // not a sensible thing to compare against — so the `contains`
            // literal is left empty for the caller to fill from the user's
            // selection (or from what they type into the prompt).
            out.push(Predicate::Contains(String::new()));
            out.push(Predicate::Exists);
        }
    }
    out
}

/// A variable name for capturing this subject, unique against `taken`.
///
/// The last path segment is what the value is called in the reply, which is
/// nearly always what the user would have typed anyway (`$.data.access_token`
/// → `access_token`).
pub fn suggest_name(subject: &Subject, taken: &[String]) -> String {
    let base = match subject {
        Subject::Status => "status".to_string(),
        Subject::Duration => "duration".to_string(),
        Subject::Header(name) => sanitise(name),
        Subject::Json(path) | Subject::JsonCount(path) => {
            let leaf = path
                .rsplit(|c| c == '.' || c == '[')
                .map(|s| s.trim_end_matches([']', '\'']).trim_start_matches('\''))
                .find(|s| !s.is_empty() && !s.bytes().all(|b| b.is_ascii_digit()))
                .unwrap_or("value");
            sanitise(leaf)
        }
        Subject::Body => "body".to_string(),
    };
    let base = if base.is_empty() {
        "value".to_string()
    } else {
        base
    };
    if !taken.iter().any(|t| t == &base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|c| !taken.iter().any(|t| t == c))
        .unwrap_or(base)
}

/// Reduce a header or key name to something usable as a `{{variable}}` name:
/// Hurl reads a placeholder only as far as the first character outside its own
/// set, so `Content-Type` as a capture name would be referenced as `Content`.
fn sanitise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    // A name may not start with a digit, or `{{2fa}}` reads as a number.
    if trimmed.starts_with(|c: char| c.is_ascii_digit()) {
        format!("v{trimmed}")
    } else {
        trimmed
    }
}

/// The innermost JSON value whose text covers `offset` in the raw body.
///
/// This is the "point at it" half: a click or a text selection in the response
/// panel is a byte offset, and this is what turns it into a path. It works off
/// the raw bytes rather than a parsed [`Value`] because `serde_json` does not
/// report spans — and because working from the source text means a *minified*
/// body (one line, no whitespace) resolves exactly as well as a pretty-printed
/// one, which a line-based mapping could not do.
///
/// Landing on a key returns that key's value, since "assert on the thing I
/// clicked the name of" is what is meant. An offset in the whitespace between
/// values, or past the end of the JSON, returns the innermost container that
/// encloses it.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn probe_at(body: &str, offset: usize) -> Option<Probe> {
    let value: Value = serde_json::from_str(body).ok()?;
    let mut spans: Vec<(Range<usize>, String)> = Vec::new();
    let mut scan = Scan {
        s: body.as_bytes(),
        i: 0,
    };
    scan.ws();
    scan.value(&mut String::from("$"), 0, &mut spans)?;
    // Innermost wins: the spans nest, so the shortest one covering the offset
    // is the most specific thing the user can have been pointing at.
    let path = spans
        .into_iter()
        .filter(|(r, _)| r.contains(&offset) || r.end == offset)
        .min_by_key(|(r, _)| r.end - r.start)
        .map(|(_, p)| p)?;
    let at = value_at(&value, &path)?;
    Some(Probe {
        subject: match at {
            Value::Array(_) => Subject::JsonCount(path),
            _ => Subject::Json(path),
        },
        value: Some(at.clone()),
    })
}

/// Follow a path built by [`push_key`] back into a parsed body.
///
/// Only paths this module produced are ever passed in, so the accepted grammar
/// is exactly what [`push_key`] emits — `.key`, `['key']`, `[0]` — rather than
/// jsonpath at large.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    let mut s = path.strip_prefix('$')?;
    while !s.is_empty() {
        if let Some(rest) = s.strip_prefix('.') {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            cur = cur.get(&rest[..end])?;
            s = &rest[end..];
        } else {
            let rest = s.strip_prefix('[')?;
            if let Some(quoted) = rest.strip_prefix('\'') {
                let (key, after) = unescape_bracket_key(quoted)?;
                cur = cur.get(&key)?;
                s = after;
            } else {
                let close = rest.find(']')?;
                cur = cur.get(rest[..close].parse::<usize>().ok()?)?;
                s = &rest[close + 1..];
            }
        }
    }
    Some(cur)
}

/// Read a `['key']` segment body, undoing [`push_key`]'s escapes, and return
/// the key plus the rest of the path after the closing `]`.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn unescape_bracket_key(s: &str) -> Option<(String, &str)> {
    let mut key = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '\\' => key.push(it.next()?.1),
            '\'' => return s[i + 1..].strip_prefix(']').map(|rest| (key, rest)),
            c => key.push(c),
        }
    }
    None
}

/// A byte scanner over JSON source that records the span of every value.
///
/// Deliberately permissive about what it accepts: the body has already been
/// parsed by `serde_json` before this runs (see [`probe_at`]), so it is known
/// to be well-formed and this pass only has to find the boundaries, not
/// validate them.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
impl Scan<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// Consume one value, pushing `(span, path)` for it and everything inside.
    /// Returns `None` on anything unexpected, which aborts the whole mapping
    /// rather than reporting a path derived from a mis-parse.
    fn value(
        &mut self,
        path: &mut String,
        depth: usize,
        out: &mut Vec<(Range<usize>, String)>,
    ) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let start = self.i;
        match *self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                loop {
                    self.ws();
                    match *self.s.get(self.i)? {
                        b'}' => {
                            self.i += 1;
                            break;
                        }
                        b',' => {
                            self.i += 1;
                            continue;
                        }
                        b'"' => {}
                        _ => return None,
                    }
                    let key_start = self.i;
                    let key = self.string()?;
                    let key_span = key_start..self.i;
                    self.ws();
                    if *self.s.get(self.i)? != b':' {
                        return None;
                    }
                    self.i += 1;
                    self.ws();
                    let mark = path.len();
                    push_key(path, &key);
                    // The key's own span maps to the value: clicking a name is
                    // how most people point at the field it names.
                    out.push((key_span, path.clone()));
                    self.value(path, depth + 1, out)?;
                    path.truncate(mark);
                }
            }
            b'[' => {
                self.i += 1;
                let mut idx = 0usize;
                loop {
                    self.ws();
                    match *self.s.get(self.i)? {
                        b']' => {
                            self.i += 1;
                            break;
                        }
                        b',' => {
                            self.i += 1;
                            continue;
                        }
                        _ => {}
                    }
                    let mark = path.len();
                    path.push_str(&format!("[{idx}]"));
                    self.value(path, depth + 1, out)?;
                    path.truncate(mark);
                    idx += 1;
                }
            }
            b'"' => {
                self.string()?;
            }
            _ => {
                // A bare token: number, `true`, `false` or `null`. It ends
                // where the structure around it resumes.
                let end = self.s[self.i..]
                    .iter()
                    .position(|b| matches!(b, b',' | b'}' | b']') || b.is_ascii_whitespace())
                    .map(|n| self.i + n)
                    .unwrap_or(self.s.len());
                if end == self.i {
                    return None;
                }
                self.i = end;
            }
        }
        out.push((start..self.i, path.clone()));
        Some(())
    }

    /// Consume a quoted string, returning its unescaped text.
    fn string(&mut self) -> Option<String> {
        if *self.s.get(self.i)? != b'"' {
            return None;
        }
        self.i += 1;
        let start = self.i;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    let raw = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                    self.i += 1;
                    // Round-tripping through `serde_json` undoes `\uXXXX` and
                    // the rest exactly as the parser did, so the key here is
                    // byte-identical to the one in the parsed `Value`.
                    return serde_json::from_str::<String>(&format!("\"{raw}\"")).ok();
                }
                _ => self.i += 1,
            }
        }
        None
    }
}

/// The verbs worth offering for a subject.
///
/// `selection` is the text highlighted in the response panel, which is the only
/// possible source of a literal for a body that isn't JSON: there is no value
/// to pre-fill from, so `body contains …` is offered only when the user has
/// already pointed at the text they mean.
pub(crate) fn verbs_for(probe: &Probe, selection: Option<&str>) -> Vec<Verb> {
    let mut out = Vec::new();
    for predicate in default_predicates(probe) {
        match (&probe.subject, &predicate) {
            (Subject::Status, Predicate::Eq(v)) => match v.parse::<u16>() {
                Ok(code) => out.push(Verb::ExpectStatus(code)),
                Err(_) => continue,
            },
            (Subject::Body, Predicate::Contains(_)) => {
                let Some(text) = selection.map(str::trim).filter(|t| !t.is_empty()) else {
                    continue;
                };
                out.push(Verb::Assert(Predicate::Contains(
                    literal(&serde_json::Value::String(text.to_string())).unwrap_or_default(),
                )));
            }
            _ => {
                // Anything Hurl has no spelling for is dropped rather than
                // shown as a row that produces nothing when chosen.
                if assert_line(probe.subject.clone(), predicate.clone()).is_some() {
                    out.push(Verb::Assert(predicate));
                }
            }
        }
    }
    // Capturing is offered wherever there is a query to capture — which is
    // everywhere except the body-as-text fallback, where the "value" is the
    // whole reply and no variable wants that.
    if !matches!(probe.subject, Subject::Body) {
        out.push(Verb::Capture);
    }
    out
}

/// How a subject reads in the picker: the jsonpath for a body value, and a
/// worded form for the parts of the exchange that aren't one.
pub(crate) fn subject_label(subject: &Subject) -> String {
    match subject {
        Subject::Json(path) => path.clone(),
        Subject::JsonCount(path) => format!("{path} []"),
        Subject::Header(name) => format!("header {name}"),
        Subject::Status => "status".to_string(),
        Subject::Duration => "duration".to_string(),
        Subject::Body => "body".to_string(),
    }
}

/// The observed value, shortened to fit a row.
///
/// Shown next to every subject because the path alone rarely settles "is this
/// the field I'm looking at?", and because the value is what the offered
/// equality assert is going to contain.
pub(crate) fn value_preview(value: Option<&serde_json::Value>, max: usize) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(m) => format!("{{{}}}", m.len()),
        serde_json::Value::Array(a) => format!("[{}]", a.len()),
        other => other.to_string(),
    };
    // One line: a preview that wraps would push the rows below it off the
    // bottom of a list whose whole job is to be scanned.
    let raw = raw.replace(['\n', '\r', '\t'], " ");
    if max == usize::MAX || raw.chars().count() <= max {
        return raw;
    }
    let head: String = raw.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// How a verb reads in step two: the assert line itself, so the row shows
/// exactly what will be written to the file.
pub(crate) fn verb_label(subject: &Subject, verb: &Verb, s: &Strings) -> String {
    match verb {
        Verb::Assert(predicate) => assert_line(subject.clone(), predicate.clone())
            .unwrap_or_else(|| s.probe_verb_unavailable.to_string()),
        Verb::ExpectStatus(code) => format!("HTTP {code}"),
        Verb::Capture => s.probe_verb_capture.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(body: &str) -> Vec<String> {
        probes(0, None, &[], body)
            .into_iter()
            .map(|p| match p.subject {
                Subject::Json(p) => p,
                Subject::JsonCount(p) => format!("{p} count"),
                other => other.query(),
            })
            .collect()
    }

    #[test]
    fn every_value_in_a_body_is_offered_in_document_order() {
        let body = r#"{"id":7,"user":{"name":"ada","tags":["a","b"]}}"#;
        assert_eq!(
            paths(body),
            [
                "$",
                "$.id",
                "$.user",
                "$.user.name",
                "$.user.tags count",
                "$.user.tags[0]",
                "$.user.tags[1]",
            ]
        );
    }

    #[test]
    fn keys_that_are_not_identifiers_are_bracket_quoted() {
        assert_eq!(
            paths(r#"{"content-type":1,"2fa":2,"it's":3}"#),
            ["$", "$['content-type']", "$['2fa']", "$['it\\'s']"]
        );
    }

    /// The status/duration/header subjects come from the exchange, not the
    /// body, and lead the list because they are what most asserts are about.
    #[test]
    fn the_exchange_itself_is_assertable() {
        let headers = [("Content-Type".to_string(), "application/json".to_string())];
        let list = probes(201, Some(42), &headers, "null");
        assert_eq!(list[0].subject, Subject::Status);
        assert_eq!(list[0].value, Some(json!(201)));
        assert_eq!(list[1].subject, Subject::Duration);
        assert_eq!(list[2].subject, Subject::Header("Content-Type".into()));
        assert_eq!(list[3].subject, Subject::Json("$".into()));
    }

    /// An HTML error page is still worth asserting on — as text.
    #[test]
    fn a_body_that_is_not_json_falls_back_to_the_text() {
        let list = probes(500, None, &[], "<html>boom</html>");
        assert_eq!(list.last().unwrap().subject, Subject::Body);
        // Nothing to say about a reply that has no body at all.
        assert!(
            probes(204, None, &[], "   ")
                .iter()
                .all(|p| p.subject != Subject::Body)
        );
    }

    #[test]
    fn a_huge_body_stops_at_the_cap() {
        let body = format!("[{}]", vec!["1"; MAX_PROBES + 500].join(","));
        assert!(probes(0, None, &[], &body).len() <= MAX_PROBES + 1);
    }

    #[test]
    fn values_are_escaped_into_hurl_literals() {
        assert_eq!(literal(&json!("a\"b\\c\nd")).unwrap(), r#""a\"b\\c\nd""#);
        assert_eq!(literal(&json!(1.5)).unwrap(), "1.5");
        assert_eq!(literal(&json!(true)).unwrap(), "true");
        assert_eq!(literal(&json!(null)).unwrap(), "null");
        // No spelling for "equals this whole object".
        assert!(literal(&json!({"a":1})).is_none());
        assert!(literal(&json!([1])).is_none());
    }

    #[test]
    fn assert_lines_read_as_hurl() {
        let eq = |s: Subject, v: &str| assert_line(s, Predicate::Eq(v.to_string()));
        assert_eq!(
            eq(Subject::Json("$.a".into()), "\"x\"").unwrap(),
            r#"jsonpath "$.a" == "x""#
        );
        assert_eq!(
            eq(Subject::JsonCount("$.a".into()), "3").unwrap(),
            r#"jsonpath "$.a" count == 3"#
        );
        assert_eq!(
            eq(Subject::Header("X-Id".into()), "\"1\"").unwrap(),
            r#"header "X-Id" == "1""#
        );
        assert_eq!(
            assert_line(Subject::Json("$.a".into()), Predicate::Empty(false)).unwrap(),
            r#"jsonpath "$.a" not isEmpty"#
        );
        // The status lives on the `HTTP <status>` line instead.
        assert!(eq(Subject::Status, "200").is_none());
        // A count is a number: text predicates do not apply to it.
        assert!(
            assert_line(
                Subject::JsonCount("$.a".into()),
                Predicate::Contains("\"x\"".into())
            )
            .is_none()
        );
        // `duration exists` is true of everything.
        assert!(assert_line(Subject::Duration, Predicate::Exists).is_none());
    }

    #[test]
    fn the_first_predicate_offered_pins_the_value_that_came_back() {
        let p = Probe {
            subject: Subject::Json("$.name".into()),
            value: Some(json!("ada")),
        };
        assert_eq!(default_predicates(&p)[0], Predicate::Eq("\"ada\"".into()));
        // Text tests are offered for text and withheld from numbers.
        assert!(default_predicates(&p).contains(&Predicate::Contains("\"ada\"".into())));
        let n = Probe {
            subject: Subject::Json("$.n".into()),
            value: Some(json!(4)),
        };
        assert!(
            !default_predicates(&n)
                .iter()
                .any(|p| matches!(p, Predicate::Contains(_)))
        );
        // A duration gets headroom, never the exact time observed.
        let d = Probe {
            subject: Subject::Duration,
            value: Some(json!(214)),
        };
        assert_eq!(default_predicates(&d), [Predicate::Lt("500".into())]);
    }

    #[test]
    fn capture_names_come_from_the_field_they_capture() {
        let taken = ["token".to_string()];
        assert_eq!(
            suggest_name(&Subject::Json("$.data.token".into()), &[]),
            "token"
        );
        assert_eq!(
            suggest_name(&Subject::Json("$.data.token".into()), &taken),
            "token2"
        );
        // An index is not a name; the key above it is.
        assert_eq!(
            suggest_name(&Subject::Json("$.items[3]".into()), &[]),
            "items"
        );
        // A header name is not a legal placeholder name as it stands.
        assert_eq!(
            suggest_name(&Subject::Header("Content-Type".into()), &[]),
            "content_type"
        );
        assert_eq!(suggest_name(&Subject::Json("$['2fa']".into()), &[]), "v2fa");
    }

    #[test]
    fn a_capture_row_is_the_query_under_a_name() {
        assert_eq!(
            capture_row(&Subject::Json("$.token".into()), "tok"),
            ("tok".to_string(), "jsonpath \"$.token\"".to_string())
        );
    }

    /// Pointing at a value is the other half of the feature; these are the
    /// offsets a click in the response panel turns into.
    #[test]
    fn an_offset_resolves_to_the_value_under_it() {
        let body = r#"{"id":7,"user":{"name":"ada","tags":["a","bb"]}}"#;
        let at = |needle: &str| {
            probe_at(body, body.find(needle).unwrap())
                .map(|p| p.subject)
                .unwrap()
        };
        assert_eq!(at("7"), Subject::Json("$.id".into()));
        assert_eq!(at("\"ada\""), Subject::Json("$.user.name".into()));
        assert_eq!(at("\"bb\""), Subject::Json("$.user.tags[1]".into()));
        // A click on the key means the field it names.
        assert_eq!(at("\"name\""), Subject::Json("$.user.name".into()));
        // An array resolves as a count, the useful assert on a list.
        assert_eq!(at("[\"a\""), Subject::JsonCount("$.user.tags".into()));
        // The whole document for an offset that is in no value in particular.
        assert_eq!(
            probe_at(body, 0).unwrap().subject,
            Subject::Json("$".into())
        );
    }

    /// A minified body has no lines to map, which is exactly why the mapping is
    /// done over byte spans rather than rows.
    #[test]
    fn pointing_works_on_a_pretty_printed_body_too() {
        let body = "{\n  \"a\": {\n    \"b\": [1, 22, 333]\n  }\n}";
        let p = probe_at(body, body.find("22").unwrap()).unwrap();
        assert_eq!(p.subject, Subject::Json("$.a.b[1]".into()));
        assert_eq!(p.value, Some(json!(22)));
    }

    #[test]
    fn pointing_at_an_awkward_key_round_trips() {
        let body = r#"{"a-b":{"c":"\u00e9 \"q\""}}"#;
        let p = probe_at(body, body.find("\\u00e9").unwrap()).unwrap();
        assert_eq!(p.subject, Subject::Json("$['a-b'].c".into()));
        assert_eq!(p.value, Some(json!("é \"q\"")));
    }

    /// Whether an `[Asserts]` line is something Hurl itself can read. A line
    /// that only PaperBoy can parse is worse than no line at all: hurl refuses
    /// the file, so every request in the collection stops running.
    fn hurl_reads(line: &str) -> Result<(), String> {
        let text = format!("GET http://h/a\nHTTP 200\n[Asserts]\n{line}\n");
        hurl_core::parser::parse_hurl_file(&text)
            .map(|_| ())
            .map_err(|e| format!("{:?} at {:?}", e.kind, e.pos))
    }

    #[test]
    fn a_key_hurl_would_choke_on_is_escaped_for_both_layers() {
        // The body is the JSON each key was read out of, kept beside it so the
        // case reads as a real response rather than an invented string.
        for (_body, key) in [
            (r#"{"a\"b":1}"#, "a\"b"),
            (r#"{"a\\b":1}"#, "a\\b"),
            (r#"{"a\nb":1}"#, "a\nb"),
        ] {
            let subject = Subject::Json({
                let mut path = "$".to_string();
                push_key(&mut path, key);
                path
            });
            let line = assert_line(subject, Predicate::Eq("1".into())).unwrap();
            assert!(hurl_reads(&line).is_ok(), "{line}: {:?}", hurl_reads(&line));
        }
    }

    #[test]
    fn a_backslash_in_a_key_reaches_the_evaluator_unchanged() {
        // The dangerous half of the same bug: this one parses, so nothing
        // complains, and the query silently selects a key that is not there.
        let mut path = "$".to_string();
        push_key(&mut path, "a\\b");
        let line = assert_line(Subject::Json(path.clone()), Predicate::Eq("1".into())).unwrap();
        let text = format!("GET http://h/a\nHTTP 200\n[Asserts]\n{line}\n");
        let file = hurl_core::parser::parse_hurl_file(&text).unwrap();
        let source = format!("{:?}", file);
        assert!(
            source.contains(&format!("value: {path:?}")),
            "the evaluator must receive the path we built, not a decoded copy: {line}"
        );
    }

    #[test]
    fn a_response_value_that_looks_like_a_placeholder_is_compared_as_text() {
        let line = assert_line(
            Subject::Json("$.greeting".into()),
            Predicate::Eq(literal(&json!("Hello {{ user.name }}")).unwrap()),
        )
        .unwrap();
        assert!(hurl_reads(&line).is_ok(), "{line}");
        let text = format!("GET http://h/a\nHTTP 200\n[Asserts]\n{line}\n");
        let file = hurl_core::parser::parse_hurl_file(&text).unwrap();
        assert!(
            !format!("{:?}", file).contains("Placeholder"),
            "the value came back as a template, not as text: {line}"
        );
    }

    #[test]
    fn an_unbalanced_brace_pair_still_leaves_a_readable_file() {
        let line = assert_line(
            Subject::Json("$.greeting".into()),
            Predicate::Eq(literal(&json!("Hello {{ user.name }")).unwrap()),
        )
        .unwrap();
        assert!(hurl_reads(&line).is_ok(), "{line}");
    }

    #[test]
    fn every_number_a_response_can_carry_becomes_a_line_hurl_reads() {
        for body in [
            r#"{"n":1.5e-3}"#,
            r#"{"n":1e10}"#,
            r#"{"n":1E+2}"#,
            r#"{"n":-2.5E3}"#,
            r#"{"n":0.0015}"#,
            r#"{"n":3.14}"#,
            r#"{"n":12345678901234567890}"#,
            r#"{"n":-0}"#,
        ] {
            let probe = probes(0, None, &[], body)
                .into_iter()
                .find(|p| matches!(&p.subject, Subject::Json(p) if p == "$.n"))
                .unwrap();
            let line =
                assert_line(probe.subject.clone(), default_predicates(&probe)[0].clone()).unwrap();
            assert!(hurl_reads(&line).is_ok(), "{body} -> {line}");
        }
    }

    #[test]
    fn an_expanded_number_still_says_what_the_response_said() {
        for (src, want) in [
            ("1.5e-3", "0.0015"),
            ("1e10", "10000000000"),
            ("1E+2", "100"),
            ("-2.5E3", "-2500"),
            ("3.14", "3.14"),
        ] {
            let v: Value = serde_json::from_str(src).unwrap();
            assert_eq!(literal(&v).as_deref(), Some(want), "{src}");
        }
    }

    #[test]
    fn a_number_too_big_to_write_out_declines_rather_than_rounding() {
        let v: Value = serde_json::from_str("1e300").unwrap();
        assert_eq!(literal(&v), None);
    }

    #[test]
    fn pointing_into_something_that_is_not_json_finds_nothing() {
        assert!(probe_at("<html>", 2).is_none());
        assert!(probe_at("", 0).is_none());
    }
}
