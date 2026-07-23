//! PaperTrail text → [`ReportFlow`] AST.
//!
//! Line-oriented and forgiving: `FOR … END` (and `REPORT REQUEST … WITH … END`)
//! delimit blocks, leading whitespace is ignored, and only whole-line `#`
//! comments exist. The one multi-line construct is a bracketed producer /
//! literal (`[ … ]`, `ZIP( … )`), whose physical lines are joined until the
//! brackets balance. Errors carry the 1-based source line so the view can point
//! at the offending statement (mirroring how unresolved `{{VAR}}`s are surfaced).
//!
//! See `docs/reports/02-grammar.md` for the grammar.

use super::flow::{
    Binder, Element, EnvClause, FlowNode, Header, HeaderLine, ParallelSpec, Pattern, Producer,
    ReportFlow, ReportStmt, ResponseFmt, WithItem,
};

/// A parse failure, with the 1-based source line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Parse PaperTrail source into a [`ReportFlow`].
pub fn parse_flow(text: &str) -> Result<ReportFlow, ParseError> {
    let phys: Vec<&str> = text.lines().collect();

    // 1. Header: `#` directives/comments that precede the first statement.
    let mut header = Header::default();
    let mut body_start = phys.len();
    for (i, raw) in phys.iter().enumerate() {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix('#') {
            let rest = rest.trim();
            match rest.split_once(':') {
                Some((k, v)) if is_ident(k.trim()) => header.lines.push(HeaderLine::Directive {
                    key: k.trim().to_string(),
                    value: v.trim().to_string(),
                }),
                _ => header.lines.push(HeaderLine::Comment(rest.to_string())),
            }
        } else {
            body_start = i;
            break;
        }
    }

    // 2. Body: fold physical lines into logical lines (joining bracketed
    //    continuations), dropping blank lines and whole-line comments.
    let logical = logical_lines(&phys, body_start)?;

    // 3. Parse the statement stream.
    let mut idx = 0;
    let nodes = parse_block(&logical, &mut idx, false)?;
    if idx < logical.len() {
        // An `END` with no matching `FOR`.
        return Err(ParseError {
            line: logical[idx].line,
            message: "unexpected END (no open FOR)".to_string(),
        });
    }
    Ok(ReportFlow { header, nodes })
}

/// A body statement after continuation-joining: its text and the 1-based line
/// it started on.
struct LogicalLine {
    line: usize,
    text: String,
}

/// Fold physical body lines into logical lines. A statement continues onto the
/// next physical line while `[`/`(` brackets opened on it stay unbalanced (so a
/// list literal or `ZIP(…)` may span lines); joined pieces are separated by a
/// space. Blank lines and whole-line `#` comments are skipped.
fn logical_lines(phys: &[&str], start: usize) -> Result<Vec<LogicalLine>, ParseError> {
    let mut out = Vec::new();
    let mut i = start;
    while i < phys.len() {
        let raw = phys[i];
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        let line = i + 1;
        let mut text = t.to_string();
        // Keep pulling in following lines until brackets balance.
        while bracket_depth(&text)? > 0 && i + 1 < phys.len() {
            i += 1;
            text.push(' ');
            text.push_str(phys[i].trim());
        }
        if bracket_depth(&text)? > 0 {
            return Err(ParseError {
                line,
                message: "unbalanced '[' or '(' (unterminated list/producer)".to_string(),
            });
        }
        out.push(LogicalLine { line, text });
        i += 1;
    }
    Ok(out)
}

/// Net `[`+`(` nesting depth of `s`, ignoring brackets inside double-quoted
/// strings. Errors on an unterminated string.
fn bracket_depth(s: &str) -> Result<i32, ParseError> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            _ => {}
        }
    }
    if in_str {
        return Err(ParseError {
            line: 0,
            message: "unterminated string".to_string(),
        });
    }
    Ok(depth)
}

// ---------------------------------------------------------------------------
// Statement-stream parsing
// ---------------------------------------------------------------------------

/// Parse statements until end-of-input or (when `stop_on_end`) a matching
/// `END`. Consumes the terminating `END`.
fn parse_block(
    lines: &[LogicalLine],
    idx: &mut usize,
    stop_on_end: bool,
) -> Result<Vec<FlowNode>, ParseError> {
    let mut nodes = Vec::new();
    while *idx < lines.len() {
        let ll = &lines[*idx];
        let kw = leading_word(&ll.text).to_uppercase();
        match kw.as_str() {
            "END" => {
                ensure_bare_end(ll)?;
                if stop_on_end {
                    *idx += 1;
                    return Ok(nodes);
                }
                return Err(ParseError {
                    line: ll.line,
                    message: "unexpected END (no open FOR)".to_string(),
                });
            }
            "FOR" => {
                *idx += 1;
                let node = parse_for(ll, lines, idx)?;
                nodes.push(node);
            }
            "PARALLEL" => {
                *idx += 1;
                let node = parse_for(ll, lines, idx)?;
                nodes.push(node);
            }
            "REPORT" => {
                *idx += 1;
                let node = parse_report(ll, lines, idx)?;
                nodes.push(FlowNode::Report(node));
            }
            "REQUEST" => {
                *idx += 1;
                let name = parse_name(after_keyword(&ll.text, "REQUEST"), ll)?;
                nodes.push(FlowNode::Request { name });
            }
            "LIST" => {
                *idx += 1;
                nodes.push(parse_list_decl(ll)?);
            }
            _ => {
                *idx += 1;
                nodes.push(parse_assign(ll)?);
            }
        }
    }
    if stop_on_end {
        return Err(ParseError {
            line: lines.last().map(|l| l.line).unwrap_or(0),
            message: "missing END for FOR".to_string(),
        });
    }
    Ok(nodes)
}

fn parse_assign(ll: &LogicalLine) -> Result<FlowNode, ParseError> {
    let (key, value) = ll.text.split_once('=').ok_or_else(|| ParseError {
        line: ll.line,
        message: format!("unrecognised statement: {}", ll.text),
    })?;
    let key = key.trim();
    if !is_ident(key) {
        return Err(ParseError {
            line: ll.line,
            message: format!("invalid assignment target '{key}' (expected an identifier)"),
        });
    }
    Ok(FlowNode::Assign {
        key: key.to_string(),
        value: value.trim().to_string(),
    })
}

fn parse_list_decl(ll: &LogicalLine) -> Result<FlowNode, ParseError> {
    let rest = after_keyword(&ll.text, "LIST");
    let (name, producer_src) = rest.split_once('=').ok_or_else(|| ParseError {
        line: ll.line,
        message: "LIST needs 'NAME = <producer>'".to_string(),
    })?;
    let name = name.trim();
    if !is_ident(name) {
        return Err(ParseError {
            line: ll.line,
            message: format!("invalid LIST name '{name}'"),
        });
    }
    let toks = tokenize(producer_src.trim(), ll.line)?;
    let mut cur = Cursor::new(&toks, ll.line);
    let producer = parse_producer(&mut cur)?;
    cur.expect_eof("after LIST producer")?;
    Ok(FlowNode::ListDecl {
        name: name.to_string(),
        producer,
    })
}

fn parse_for(
    head: &LogicalLine,
    lines: &[LogicalLine],
    idx: &mut usize,
) -> Result<FlowNode, ParseError> {
    let toks = tokenize(head.text.trim(), head.line)?;
    let mut cur = Cursor::new(&toks, head.line);
    // Optional `PARALLEL[(n)]` prefix, then the mandatory `FOR`.
    let parallel = parse_parallel_prefix(&mut cur)?;
    cur.expect_word("FOR")?;
    let pattern = parse_pattern(&mut cur)?;
    cur.expect_word("IN")?;

    if cur.peek_word_is("ENVS") {
        cur.next();
        // ENVS binds a single variable.
        if !pattern.is_single() {
            return Err(ParseError {
                line: head.line,
                message: "an ENVS loop binds a single variable (e.g. FOR TARGET IN ENVS …)"
                    .to_string(),
            });
        }
        let var = match &pattern.binders[0] {
            Binder::Named(n) => n.clone(),
            Binder::Discard => {
                return Err(ParseError {
                    line: head.line,
                    message: "an ENVS loop variable can't be '_'".to_string(),
                });
            }
        };
        let clause = parse_env_clause(&mut cur)?;
        cur.expect_eof("after ENVS clause")?;
        let body = parse_block(lines, idx, true)?;
        Ok(FlowNode::ForEnvs {
            var,
            clause,
            body,
            parallel,
        })
    } else {
        let producer = parse_producer(&mut cur)?;
        cur.expect_eof("after FOR producer")?;
        let body = parse_block(lines, idx, true)?;
        Ok(FlowNode::ForEach {
            pattern,
            producer,
            body,
            parallel,
        })
    }
}

/// Parse an optional `PARALLEL` / `PARALLEL(n)` loop prefix. Returns `None` when
/// the line has no such prefix (leaving the cursor at `FOR`).
fn parse_parallel_prefix(cur: &mut Cursor) -> Result<Option<ParallelSpec>, ParseError> {
    if !cur.peek_word_is("PARALLEL") {
        return Ok(None);
    }
    cur.next(); // consume PARALLEL
    let mut degree = None;
    if cur.peek_is(&Tok::LParen) {
        cur.next();
        let n = cur.expect_word_any("a PARALLEL worker count, e.g. PARALLEL(8)")?;
        let parsed: u32 = n
            .parse()
            .map_err(|_| cur.err(&format!("invalid PARALLEL worker count '{n}'")))?;
        if parsed == 0 {
            return Err(cur.err("PARALLEL worker count must be at least 1"));
        }
        cur.expect(&Tok::RParen, "')' to close PARALLEL(n)")?;
        degree = Some(parsed);
    }
    Ok(Some(ParallelSpec { degree }))
}

fn parse_report(
    head: &LogicalLine,
    lines: &[LogicalLine],
    idx: &mut usize,
) -> Result<ReportStmt, ParseError> {
    let rest = after_keyword(&head.text, "REPORT");
    let toks = tokenize(rest, head.line)?;
    let mut cur = Cursor::new(&toks, head.line);

    if cur.peek_word_is("REQUEST") {
        cur.next();
        let name = cur.expect_name("a request name after REPORT REQUEST")?;
        let mut alias = None;
        let mut response_fmt = None;
        let mut has_with = false;
        loop {
            if cur.peek_word_is("AS") {
                cur.next();
                alias = Some(cur.expect_name("an alias after AS")?);
            } else if cur.peek_word_is("RESPONSE") {
                cur.next();
                response_fmt = Some(parse_response_fmt(&mut cur)?);
            } else if cur.peek_word_is("WITH") {
                cur.next();
                has_with = true;
                break;
            } else {
                break;
            }
        }
        cur.expect_eof("after REPORT REQUEST")?;
        let with = if has_with {
            parse_with_block(lines, idx, head.line)?
        } else {
            Vec::new()
        };
        Ok(ReportStmt::Request {
            name,
            alias,
            response_fmt,
            with,
        })
    } else if cur.peek_is(&Tok::LParen) {
        // REPORT (v1, v2, …)
        cur.next();
        let mut vars = Vec::new();
        loop {
            vars.push(cur.expect_ident("a variable name")?);
            if cur.peek_is(&Tok::Comma) {
                cur.next();
                continue;
            }
            break;
        }
        cur.expect(&Tok::RParen, "')' after REPORT variables")?;
        cur.expect_eof("after REPORT (...)")?;
        Ok(ReportStmt::Vars(vars))
    } else if let Some(template) = cur.peek_str() {
        // REPORT "template" AS name
        cur.next();
        cur.expect_word("AS")?;
        let name = cur.expect_name("a column name after AS")?;
        cur.expect_eof("after REPORT computed column")?;
        Ok(ReportStmt::Computed { template, name })
    } else {
        // REPORT <var>
        let var = cur.expect_ident("a variable name after REPORT")?;
        cur.expect_eof("after REPORT variable")?;
        Ok(ReportStmt::Vars(vec![var]))
    }
}

/// Parse the items of a `REPORT REQUEST … WITH … END` block, consuming the
/// terminating `END`.
fn parse_with_block(
    lines: &[LogicalLine],
    idx: &mut usize,
    head_line: usize,
) -> Result<Vec<WithItem>, ParseError> {
    let mut items = Vec::new();
    while *idx < lines.len() {
        let ll = &lines[*idx];
        if leading_word(&ll.text).eq_ignore_ascii_case("END") {
            ensure_bare_end(ll)?;
            *idx += 1;
            return Ok(items);
        }
        if leading_word(&ll.text).eq_ignore_ascii_case("RESPONSE") {
            let toks = tokenize(after_keyword(&ll.text, "RESPONSE"), ll.line)?;
            let mut cur = Cursor::new(&toks, ll.line);
            let fmt = parse_response_fmt_word(&mut cur)?;
            cur.expect_eof("after RESPONSE")?;
            items.push(WithItem::ResponseFmt(fmt));
        } else {
            let (name, query) = ll.text.split_once(':').ok_or_else(|| ParseError {
                line: ll.line,
                message: "WITH item must be 'name: query' or 'RESPONSE RAW|PRETTY'".to_string(),
            })?;
            let name = name.trim();
            if !is_ident(name) {
                return Err(ParseError {
                    line: ll.line,
                    message: format!("invalid report field name '{name}'"),
                });
            }
            let query = query.trim();
            if query.is_empty() {
                return Err(ParseError {
                    line: ll.line,
                    message: format!("report field '{name}' has an empty query"),
                });
            }
            items.push(WithItem::Field {
                name: name.to_string(),
                query: query.to_string(),
            });
        }
        *idx += 1;
    }
    Err(ParseError {
        line: head_line,
        message: "missing END for WITH block".to_string(),
    })
}

fn parse_response_fmt(cur: &mut Cursor) -> Result<ResponseFmt, ParseError> {
    parse_response_fmt_word(cur)
}

fn parse_response_fmt_word(cur: &mut Cursor) -> Result<ResponseFmt, ParseError> {
    if cur.peek_word_is("RAW") {
        cur.next();
        Ok(ResponseFmt::Raw)
    } else if cur.peek_word_is("PRETTY") {
        cur.next();
        Ok(ResponseFmt::Pretty)
    } else {
        Err(cur.err("expected RAW or PRETTY after RESPONSE"))
    }
}

fn parse_pattern(cur: &mut Cursor) -> Result<Pattern, ParseError> {
    if cur.peek_is(&Tok::LParen) {
        cur.next();
        let mut binders = Vec::new();
        let mut rest = false;
        loop {
            if cur.peek_is(&Tok::RParen) {
                break;
            }
            let w = cur.expect_word_any("a binder, '_' or '...'")?;
            if w == "..." {
                rest = true;
                // '...' must be the final element.
                break;
            } else if w == "_" {
                binders.push(Binder::Discard);
            } else if is_ident(&w) {
                binders.push(Binder::Named(w));
            } else {
                return Err(cur.err(&format!("invalid binder '{w}'")));
            }
            if cur.peek_is(&Tok::Comma) {
                cur.next();
                continue;
            }
            break;
        }
        cur.expect(&Tok::RParen, "')' to close the pattern")?;
        if binders.is_empty() && !rest {
            return Err(cur.err("empty destructuring pattern"));
        }
        Ok(Pattern { binders, rest })
    } else {
        let w = cur.expect_word_any("a loop variable")?;
        if w == "_" {
            Ok(Pattern {
                binders: vec![Binder::Discard],
                rest: false,
            })
        } else if is_ident(&w) {
            Ok(Pattern::single(w))
        } else {
            Err(cur.err(&format!("invalid loop variable '{w}'")))
        }
    }
}

fn parse_producer(cur: &mut Cursor) -> Result<Producer, ParseError> {
    if cur.peek_is(&Tok::LBrack) {
        return parse_list_literal(cur);
    }
    let w = cur
        .peek_word()
        .ok_or_else(|| cur.err("expected a producer"))?;
    match w.to_uppercase().as_str() {
        "FILES" => {
            cur.next();
            let dir = cur.expect_str("a directory path after FILES")?;
            let glob = if cur.peek_word_is("MATCH") {
                cur.next();
                Some(cur.expect_str("a glob after MATCH")?)
            } else {
                None
            };
            Ok(Producer::Files { dir, glob })
        }
        "FOLDERS" => {
            cur.next();
            let dir = cur.expect_str("a directory path after FOLDERS")?;
            let mut roles = Vec::new();
            if cur.peek_word_is("WITH") {
                cur.next();
                loop {
                    let role = cur.expect_ident("a role name")?;
                    cur.expect(&Tok::Eq, "'=' after role name")?;
                    let glob = cur.expect_str("a glob for the role")?;
                    roles.push((role, glob));
                    if cur.peek_is(&Tok::Comma) {
                        cur.next();
                        continue;
                    }
                    break;
                }
            }
            Ok(Producer::Folders { dir, roles })
        }
        "TUPLES" => {
            cur.next();
            cur.expect_word("FROM")?;
            let path = cur.expect_str("a manifest path after TUPLES FROM")?;
            Ok(Producer::Tuples { path })
        }
        "ZIP" => {
            cur.next();
            cur.expect(&Tok::LParen, "'(' after ZIP")?;
            let mut ps = Vec::new();
            loop {
                ps.push(parse_producer(cur)?);
                if cur.peek_is(&Tok::Comma) {
                    cur.next();
                    continue;
                }
                break;
            }
            cur.expect(&Tok::RParen, "')' to close ZIP")?;
            Ok(Producer::Zip(ps))
        }
        "JOIN" => Err(cur.err("JOIN is reserved but not yet supported")),
        "ENVS" => Err(cur.err("ENVS can only be used as 'FOR <var> IN ENVS …'")),
        _ => {
            // A named LIST reference.
            let name = cur.expect_ident("a producer or list name")?;
            Ok(Producer::Named(name))
        }
    }
}

fn parse_list_literal(cur: &mut Cursor) -> Result<Producer, ParseError> {
    cur.expect(&Tok::LBrack, "'['")?;
    let mut elems = Vec::new();
    loop {
        if cur.peek_is(&Tok::RBrack) {
            break;
        }
        if cur.peek_is(&Tok::LParen) {
            cur.next();
            let mut items = Vec::new();
            loop {
                items.push(cur.expect_str_or_word("a tuple element")?);
                if cur.peek_is(&Tok::Comma) {
                    cur.next();
                    continue;
                }
                break;
            }
            cur.expect(&Tok::RParen, "')' to close a tuple")?;
            elems.push(Element::Tuple(items));
        } else {
            elems.push(Element::Scalar(cur.expect_str_or_word("a list element")?));
        }
        if cur.peek_is(&Tok::Comma) {
            cur.next();
            continue;
        }
        break;
    }
    cur.expect(&Tok::RBrack, "']' to close the list")?;
    Ok(Producer::List(elems))
}

fn parse_env_clause(cur: &mut Cursor) -> Result<EnvClause, ParseError> {
    // Role list if the first token is BASELINE/COMPARISON; else a plain list.
    if cur.peek_word_is("BASELINE") || cur.peek_word_is("COMPARISON") {
        let mut baseline = Vec::new();
        let mut comparisons = Vec::new();
        loop {
            let role = cur
                .expect_word_any("BASELINE or COMPARISON")?
                .to_uppercase();
            cur.expect(&Tok::LParen, "'(' after role")?;
            let mut names = Vec::new();
            loop {
                names.push(cur.expect_str("an environment name")?);
                if cur.peek_is(&Tok::Comma) {
                    cur.next();
                    continue;
                }
                break;
            }
            cur.expect(&Tok::RParen, "')' to close the role")?;
            match role.as_str() {
                "BASELINE" => baseline.extend(names),
                "COMPARISON" => comparisons.extend(names),
                _ => return Err(cur.err("expected BASELINE or COMPARISON")),
            }
            if cur.peek_is(&Tok::Comma) {
                cur.next();
                // A plain name mixed in among roles is an error.
                if !(cur.peek_word_is("BASELINE") || cur.peek_word_is("COMPARISON")) {
                    return Err(
                        cur.err("cannot mix BASELINE/COMPARISON with plain environment names")
                    );
                }
                continue;
            }
            break;
        }
        Ok(EnvClause::Roles {
            baseline,
            comparisons,
        })
    } else {
        let mut names = Vec::new();
        loop {
            names.push(cur.expect_str("an environment name")?);
            if cur.peek_is(&Tok::Comma) {
                cur.next();
                continue;
            }
            break;
        }
        Ok(EnvClause::Plain(names))
    }
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

/// The leading identifier-ish run at the start of `s` (letters/digits/`_`),
/// used only to dispatch on the statement keyword.
fn leading_word(s: &str) -> &str {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// The text after a leading keyword (case-insensitive), trimmed.
fn after_keyword<'a>(s: &'a str, kw: &str) -> &'a str {
    let s = s.trim_start();
    s[kw.len().min(s.len())..].trim()
}

/// A `REQUEST`/`REPORT REQUEST` name from rest-of-line: a quoted string, or a
/// bare token (which may contain `/` for virtual folders) with no spaces.
fn parse_name(rest: &str, ll: &LogicalLine) -> Result<String, ParseError> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(ParseError {
            line: ll.line,
            message: "expected a request name".to_string(),
        });
    }
    if rest.starts_with('"') {
        let toks = tokenize(rest, ll.line)?;
        let mut cur = Cursor::new(&toks, ll.line);
        let name = cur.expect_str("a quoted request name")?;
        cur.expect_eof("after request name")?;
        Ok(name)
    } else if rest.chars().any(char::is_whitespace) {
        Err(ParseError {
            line: ll.line,
            message: "quote request names that contain spaces".to_string(),
        })
    } else {
        Ok(rest.to_string())
    }
}

fn ensure_bare_end(ll: &LogicalLine) -> Result<(), ParseError> {
    if ll.text.trim().eq_ignore_ascii_case("END") {
        Ok(())
    } else {
        Err(ParseError {
            line: ll.line,
            message: "END takes no arguments".to_string(),
        })
    }
}

/// A valid PaperTrail identifier: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Tokenizer + cursor (used for FOR / LIST / REPORT / producer syntax)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Str(String),
    LParen,
    RParen,
    LBrack,
    RBrack,
    Comma,
    Eq,
}

/// Tokenize one logical line's syntax portion. Barewords run until whitespace
/// or one of `()[],="`; quoted strings honour `\"`/`\\`.
fn tokenize(s: &str, line: usize) -> Result<Vec<Tok>, ParseError> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '[' => {
                chars.next();
                toks.push(Tok::LBrack);
            }
            ']' => {
                chars.next();
                toks.push(Tok::RBrack);
            }
            ',' => {
                chars.next();
                toks.push(Tok::Comma);
            }
            '=' => {
                chars.next();
                toks.push(Tok::Eq);
            }
            '"' => {
                chars.next();
                let mut buf = String::new();
                let mut closed = false;
                while let Some(ch) = chars.next() {
                    match ch {
                        '\\' => match chars.next() {
                            Some('"') => buf.push('"'),
                            Some('\\') => buf.push('\\'),
                            Some(other) => {
                                buf.push('\\');
                                buf.push(other);
                            }
                            None => break,
                        },
                        '"' => {
                            closed = true;
                            break;
                        }
                        _ => buf.push(ch),
                    }
                }
                if !closed {
                    return Err(ParseError {
                        line,
                        message: "unterminated string".to_string(),
                    });
                }
                toks.push(Tok::Str(buf));
            }
            _ => {
                let mut buf = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || matches!(ch, '(' | ')' | '[' | ']' | ',' | '=' | '"') {
                        break;
                    }
                    buf.push(ch);
                    chars.next();
                }
                toks.push(Tok::Word(buf));
            }
        }
    }
    Ok(toks)
}

struct Cursor<'a> {
    toks: &'a [Tok],
    pos: usize,
    line: usize,
}

impl<'a> Cursor<'a> {
    fn new(toks: &'a [Tok], line: usize) -> Self {
        Cursor { toks, pos: 0, line }
    }

    fn err(&self, msg: &str) -> ParseError {
        ParseError {
            line: self.line,
            message: msg.to_string(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn peek_is(&self, t: &Tok) -> bool {
        self.peek() == Some(t)
    }

    fn peek_word(&self) -> Option<String> {
        match self.peek() {
            Some(Tok::Word(w)) => Some(w.clone()),
            _ => None,
        }
    }

    fn peek_word_is(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(kw))
    }

    fn peek_str(&self) -> Option<String> {
        match self.peek() {
            Some(Tok::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }

    fn expect(&mut self, t: &Tok, what: &str) -> Result<(), ParseError> {
        if self.peek_is(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected {what}")))
        }
    }

    fn expect_word(&mut self, kw: &str) -> Result<(), ParseError> {
        if self.peek_word_is(kw) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{kw}'")))
        }
    }

    /// Any bareword token (returns its text).
    fn expect_word_any(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            _ => Err(self.err(&format!("expected {what}"))),
        }
    }

    /// A valid identifier bareword.
    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        let w = self.expect_word_any(what)?;
        if is_ident(&w) {
            Ok(w)
        } else {
            Err(self.err(&format!("expected {what}, got '{w}'")))
        }
    }

    /// A quoted string.
    fn expect_str(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err(self.err(&format!("expected {what} (a quoted string)"))),
        }
    }

    /// A quoted string or a bareword (list elements accept either).
    fn expect_str_or_word(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            _ => Err(self.err(&format!("expected {what}"))),
        }
    }

    /// A request/column name: a quoted string or a bareword.
    fn expect_name(&mut self, what: &str) -> Result<String, ParseError> {
        self.expect_str_or_word(what)
    }

    fn expect_eof(&self, what: &str) -> Result<(), ParseError> {
        if self.pos >= self.toks.len() {
            Ok(())
        } else {
            Err(self.err(&format!("unexpected extra input {what}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::flow::{
        Binder, Element, EnvClause, FlowNode, ParallelSpec, Producer, ReportStmt, WithItem,
    };

    /// Parse, then serialize, then parse again — the two ASTs must match, so the
    /// text form and the AST are faithful inverses.
    fn assert_round_trips(src: &str) -> ReportFlow {
        let flow = parse_flow(src).expect("parse should succeed");
        let text = flow.to_text();
        let reparsed = parse_flow(&text)
            .unwrap_or_else(|e| panic!("re-parse failed: {e}\n--- serialized ---\n{text}"));
        assert_eq!(flow, reparsed, "round-trip changed the AST:\n{text}");
        flow
    }

    #[test]
    fn minimal_flow_parses_and_round_trips() {
        let flow = assert_round_trips(
            "# name: Smoke\n# collection: ./c.hurl\n\nURL=some.url:8080/api\nREQUEST Oauth\nREPORT REQUEST process_file\n",
        );
        assert_eq!(flow.header.collection(), Some("./c.hurl"));
        assert_eq!(flow.header.name(), Some("Smoke"));
        assert!(matches!(flow.nodes[0], FlowNode::Assign { .. }));
        assert!(matches!(flow.nodes[1], FlowNode::Request { .. }));
        assert!(matches!(flow.nodes[2], FlowNode::Report(_)));
    }

    #[test]
    fn assignment_value_is_rest_of_line_untokenized() {
        let flow = parse_flow("URL=http://x:8080/a?b=c&d=e\n").unwrap();
        match &flow.nodes[0] {
            FlowNode::Assign { key, value } => {
                assert_eq!(key, "URL");
                assert_eq!(value, "http://x:8080/a?b=c&d=e");
            }
            other => panic!("expected assign, got {other:?}"),
        }
    }

    #[test]
    fn files_loop_with_match_glob() {
        let flow = assert_round_trips(
            "FOR FILE IN FILES \"images/real\" MATCH \"*.jpg\"\n    REPORT REQUEST up\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::ForEach {
                pattern, producer, ..
            } => {
                assert!(pattern.is_single());
                assert_eq!(
                    producer,
                    &Producer::Files {
                        dir: "images/real".into(),
                        glob: Some("*.jpg".into())
                    }
                );
            }
            other => panic!("expected ForEach, got {other:?}"),
        }
    }

    #[test]
    fn nested_loops_round_trip() {
        assert_round_trips(
            "FOR FRONT IN FILES \"fronts\"\n    FOR BACK IN FILES \"backs\"\n        REPORT REQUEST m\n    END\nEND\n",
        );
    }

    #[test]
    fn list_and_tuple_destructuring_spanning_lines() {
        let flow = assert_round_trips(
            "LIST DOCUMENTS = [(\"f1\", \"b1\"), (\"f2\", \"b2\")]\nFOR (FRONT, BACK) IN DOCUMENTS\n    REPORT REQUEST ul\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::ListDecl { name, producer } => {
                assert_eq!(name, "DOCUMENTS");
                assert_eq!(
                    producer,
                    &Producer::List(vec![
                        Element::Tuple(vec!["f1".into(), "b1".into()]),
                        Element::Tuple(vec!["f2".into(), "b2".into()]),
                    ])
                );
            }
            other => panic!("expected ListDecl, got {other:?}"),
        }
    }

    #[test]
    fn multiline_list_literal_is_joined() {
        let flow = parse_flow(
            "LIST DOCS = [\n  (\"a\", \"b\"),\n  (\"c\", \"d\")\n]\nFOR (X, Y) IN DOCS\n  REQUEST r\nEND\n",
        )
        .unwrap();
        assert!(matches!(&flow.nodes[0], FlowNode::ListDecl { .. }));
    }

    #[test]
    fn destructuring_escapes_parse() {
        let flow = parse_flow("FOR (FRONT, BACK, _) IN DOCS\n  REQUEST r\nEND\n").unwrap();
        if let FlowNode::ForEach { pattern, .. } = &flow.nodes[0] {
            assert_eq!(pattern.binders.len(), 3);
            assert!(matches!(pattern.binders[2], Binder::Discard));
            assert!(!pattern.rest);
        } else {
            panic!("expected ForEach");
        }
        let flow2 = parse_flow("FOR (FRONT, BACK, ...) IN DOCS\n  REQUEST r\nEND\n").unwrap();
        if let FlowNode::ForEach { pattern, .. } = &flow2.nodes[0] {
            assert_eq!(pattern.binders.len(), 2);
            assert!(pattern.rest);
        } else {
            panic!("expected ForEach");
        }
    }

    #[test]
    fn folders_with_roles_and_zip_and_tuples() {
        assert_round_trips(
            "FOR CASE IN FOLDERS \"cases\" WITH front=\"*_front.*\", back=\"*_back.*\"\n    REQUEST r\nEND\n",
        );
        assert_round_trips(
            "LIST PAIRS = ZIP(FILES \"fronts\" MATCH \"*.jpg\", FILES \"backs\")\nFOR (F, B) IN PAIRS\n    REQUEST r\nEND\n",
        );
        assert_round_trips("FOR ROW IN TUPLES FROM \"m.csv\"\n    REQUEST r\nEND\n");
    }

    #[test]
    fn envs_plain_and_roles() {
        let plain = parse_flow("FOR T IN ENVS \"au\", \"eu\"\n  REQUEST r\nEND\n").unwrap();
        if let FlowNode::ForEnvs { var, clause, .. } = &plain.nodes[0] {
            assert_eq!(var, "T");
            assert_eq!(clause, &EnvClause::Plain(vec!["au".into(), "eu".into()]));
        } else {
            panic!("expected ForEnvs");
        }
        let roles = assert_round_trips(
            "FOR TARGET IN ENVS BASELINE(\"prod-au\"), COMPARISON(\"staging-au\", \"staging-eu\")\n    REQUEST r\nEND\n",
        );
        if let FlowNode::ForEnvs { clause, .. } = &roles.nodes[0] {
            assert_eq!(
                clause,
                &EnvClause::Roles {
                    baseline: vec!["prod-au".into()],
                    comparisons: vec!["staging-au".into(), "staging-eu".into()],
                }
            );
        } else {
            panic!("expected ForEnvs roles");
        }
    }

    #[test]
    fn report_forms_round_trip() {
        // AS alias + RESPONSE + WITH block
        let flow = assert_round_trips(
            "FOR FILE IN FILES \"d\"\n    REPORT REQUEST process AS proc RESPONSE RAW WITH\n        RESPONSE PRETTY\n        overall: jsonpath \"$.overall_result\"\n    END\n    REPORT (FILE)\n    REPORT \"run {{URL}}\" AS note\nEND\n",
        );
        if let FlowNode::ForEach { body, .. } = &flow.nodes[0] {
            match &body[0] {
                FlowNode::Report(ReportStmt::Request {
                    alias,
                    with,
                    response_fmt,
                    ..
                }) => {
                    assert_eq!(alias.as_deref(), Some("proc"));
                    assert!(response_fmt.is_some());
                    assert_eq!(with.len(), 2);
                    assert!(matches!(with[0], WithItem::ResponseFmt(_)));
                    assert!(matches!(with[1], WithItem::Field { .. }));
                }
                other => panic!("expected report request, got {other:?}"),
            }
            assert!(matches!(&body[1], FlowNode::Report(ReportStmt::Vars(_))));
            assert!(matches!(
                &body[2],
                FlowNode::Report(ReportStmt::Computed { .. })
            ));
        } else {
            panic!("expected ForEach");
        }
    }

    #[test]
    fn quoted_request_name_with_spaces() {
        let flow = parse_flow("REQUEST \"My Request\"\n").unwrap();
        assert_eq!(
            flow.nodes[0],
            FlowNode::Request {
                name: "My Request".into()
            }
        );
    }

    #[test]
    fn virtual_folder_request_name_is_bareword() {
        let flow = parse_flow("REQUEST auth/Oauth\n").unwrap();
        assert_eq!(
            flow.nodes[0],
            FlowNode::Request {
                name: "auth/Oauth".into()
            }
        );
    }

    #[test]
    fn unbalanced_for_is_an_error() {
        assert!(parse_flow("FOR X IN FILES \"d\"\n  REQUEST r\n").is_err());
    }

    #[test]
    fn stray_end_is_an_error() {
        assert!(parse_flow("REQUEST r\nEND\n").is_err());
    }

    #[test]
    fn reserved_join_is_rejected() {
        let err = parse_flow("FOR X IN JOIN ON \"k\" (a, b)\n  REQUEST r\nEND\n").unwrap_err();
        assert!(err.message.to_lowercase().contains("join"), "{err}");
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(parse_flow("FOR X IN FILES \"oops\n  REQUEST r\nEND\n").is_err());
    }

    #[test]
    fn error_reports_a_line_number() {
        let err = parse_flow("REQUEST ok\nFOR\n").unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn parallel_loop_default_degree() {
        let flow = assert_round_trips(
            "PARALLEL FOR FILE IN FILES \"docs\" MATCH \"*.jpg\"\n    REQUEST create_session\n    REQUEST upload_file\n    REPORT REQUEST process_file\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::ForEach { parallel, .. } => {
                assert_eq!(parallel, &Some(ParallelSpec { degree: None }));
            }
            other => panic!("expected ForEach, got {other:?}"),
        }
    }

    #[test]
    fn parallel_loop_with_explicit_degree() {
        let flow = assert_round_trips(
            "PARALLEL(8) FOR FILE IN FILES \"docs\"\n    REPORT REQUEST process\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::ForEach { parallel, .. } => {
                assert_eq!(parallel, &Some(ParallelSpec { degree: Some(8) }));
            }
            other => panic!("expected ForEach, got {other:?}"),
        }
    }

    #[test]
    fn sequential_loop_has_no_parallel_marker() {
        let flow = parse_flow("FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n").unwrap();
        match &flow.nodes[0] {
            FlowNode::ForEach { parallel, .. } => assert_eq!(parallel, &None),
            other => panic!("expected ForEach, got {other:?}"),
        }
    }

    #[test]
    fn parallel_applies_to_envs_loops_too() {
        let flow = assert_round_trips(
            "PARALLEL(4) FOR TARGET IN ENVS \"au\", \"eu\"\n    REPORT REQUEST r\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::ForEnvs { parallel, .. } => {
                assert_eq!(parallel, &Some(ParallelSpec { degree: Some(4) }));
            }
            other => panic!("expected ForEnvs, got {other:?}"),
        }
    }

    #[test]
    fn parallel_nests_inside_a_sequential_loop() {
        let flow = assert_round_trips(
            "FOR ENV IN ENVS \"au\"\n    PARALLEL FOR FILE IN FILES \"docs\"\n        REPORT REQUEST p\n    END\nEND\n",
        );
        if let FlowNode::ForEnvs { body, parallel, .. } = &flow.nodes[0] {
            assert_eq!(parallel, &None);
            assert!(matches!(
                &body[0],
                FlowNode::ForEach {
                    parallel: Some(ParallelSpec { degree: None }),
                    ..
                }
            ));
        } else {
            panic!("expected outer ForEnvs");
        }
    }

    #[test]
    fn parallel_zero_workers_is_an_error() {
        let err =
            parse_flow("PARALLEL(0) FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n").unwrap_err();
        assert!(err.message.to_lowercase().contains("at least 1"), "{err}");
    }

    #[test]
    fn parallel_non_numeric_degree_is_an_error() {
        assert!(
            parse_flow("PARALLEL(lots) FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n").is_err()
        );
    }

    #[test]
    fn parallel_without_for_is_an_error() {
        assert!(parse_flow("PARALLEL REQUEST r\n").is_err());
    }
}
