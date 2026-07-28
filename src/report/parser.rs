//! PaperTrail source text → [`ReportFlow`] AST, via a `nom` combinator
//! grammar. The full grammar is documented in the block comment below.

use nom::{
    IResult,
    branch::alt,
    bytes::complete::{take_while, take_while1},
    character::complete::{char, multispace0, multispace1, not_line_ending, satisfy},
    combinator::{eof, map, opt, recognize, value, verify},
    multi::{many0, separated_list0, separated_list1},
    sequence::{delimited, pair, preceded, separated_pair},
};

use crate::report::flow::{
    Binder, Element, EnvClause, FlowNode, Header, HeaderLine, ParallelSpec, Pattern, Producer,
    ReportFlow, ReportStmt, ResponseFmt, WithItem,
};

/*
GRAMMAR:

### Header directives

```
# collection: <ref>      (required)
# name: <text>           ({time} -> local YYYY-MM-DD-HHMMSS in written files)
# output: csv
# columns: <column-spec-list>
# root: <dir>
# environment: <loaded-env-name>
# baseline: <path-to-.baseline>
```

### EBNF

```
flow         := header-line* statement*
header-line  := '#' [ key ':' value ]                 # before the first statement

statement    := assign
              | list-decl
              | request
              | report
              | for-each
              | for-envs
              | end

assign       := IDENT '=' value                       # incl. PRELUDE_* settings
list-decl    := 'LIST' IDENT '=' producer

request      := 'REQUEST' name
report       := 'REPORT' report-target
report-target:= 'REQUEST' name [ 'AS' name ] [ response-fmt ] [ show ] [ with-block ]
              | IDENT 'AS' name                          # renamed variable column
              | var-list
              | string 'AS' name                         # computed column
var-list     := IDENT | '(' IDENT (',' IDENT)* ')'
response-fmt := 'RESPONSE' ('RAW' | 'PRETTY')
show         := 'SHOW' '(' IDENT (',' IDENT)* ')'
with-block   := 'WITH' with-item* 'END'
with-item    := response-fmt | field-def
field-def    := IDENT ':' hurl-query                   # full Hurl query + filters

for-each     := [ parallel ] 'FOR' pattern 'IN' producer statement* 'END'
for-envs     := [ parallel ] 'FOR' IDENT 'IN' 'ENVS' env-clause statement* 'END'
parallel     := 'PARALLEL' [ '(' UINT ')' ]            # concurrent iterations
end          := 'END'                                  # closes the nearest FOR

producer     := list-literal | files-src | folders-src | tuples-src | zip-src | concat-src | IDENT
files-src    := 'FILES' string [ 'MATCH' string ]
folders-src  := 'FOLDERS' string [ 'WITH' role-binding (',' role-binding)* ]
role-binding := IDENT '=' string
tuples-src   := 'TUPLES' 'FROM' string
zip-src      := 'ZIP' '(' producer (',' producer)* ')'
concat-src   := 'CONCAT' '(' producer (',' producer)* ')'
list-literal := '[' element (',' element)* ']'         # may span lines
element      := string | tuple
tuple        := '(' string (',' string)* ')'

pattern      := binder | '(' binder (',' binder)* [ ',' '...' ] ')'
binder       := IDENT | '_'

env-clause   := name-list | role-list
role-list    := role (',' role)*
role         := ('BASELINE' | 'COMPARISON') '(' name-list ')'
name-list    := string (',' string)*

name         := string | bareword                      # request / column name
value        := string | rest-of-line-trimmed
```

### `columns:` sub-grammar

```
columns      := column-spec (',' column-spec)*
column-spec  := source ('|' source)* [ 'AS' name ]     # '|' = coalesce, first non-empty
source       := IDENT ('.' IDENT)?                      # var | alias.field
name         := string | bareword
```
 */

// The whole language is scannerless: every token parser skips *leading*
// horizontal + vertical whitespace, so sequences compose without explicit
// separators, and a bracketed producer/list can span lines for free (the
// whitespace between `[` … `]` is just eaten). The only line-sensitive
// productions are the three "rest of line" values — a header directive, an
// `IDENT = value` assignment, and a `WITH` field query — which stop at the
// newline via `not_line_ending`.

/// A recoverable error at `i` (position is only used to point re-parse/leftover
/// checks at the offending text).
fn perr(i: &str) -> nom::Err<nom::error::Error<&str>> {
    nom::Err::Error(nom::error::Error::new(i, nom::error::ErrorKind::Verify))
}

/// `[A-Za-z_][A-Za-z0-9_]*` — the identifier predicate, used for header keys,
/// assignment targets and binders.
fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(x) if x.is_ascii_alphabetic() || x == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// A bareword token: everything up to whitespace or one of `()[],="` (so it may
/// contain `/`, `.`, `*`, `...`, etc). Skips leading whitespace first.
fn word(i: &str) -> IResult<&str, &str> {
    preceded(
        multispace0,
        take_while1(|c: char| !c.is_whitespace() && !"()[],=\"".contains(c)),
    )(i)
}

/// Match a specific keyword as a *whole* bareword (case-insensitive), so `FOR`
/// doesn't shadow `FORMAT`.
fn kw<'a>(k: &'static str) -> impl FnMut(&'a str) -> IResult<&'a str, ()> {
    move |i| {
        let (rest, w) = word(i)?;
        if w.eq_ignore_ascii_case(k) {
            Ok((rest, ()))
        } else {
            Err(perr(i))
        }
    }
}

/// A single punctuation char, skipping leading whitespace.
fn sym<'a>(c: char) -> impl FnMut(&'a str) -> IResult<&'a str, char> {
    move |i| preceded(multispace0, char(c))(i)
}

/// An identifier (`is_ident`), skipping leading whitespace, returned owned.
fn ident(i: &str) -> IResult<&str, String> {
    let (i, _) = multispace0(i)?;
    let (rest, s) = recognize(pair(
        satisfy(|c| c.is_ascii_alphabetic() || c == '_'),
        take_while(|c: char| c.is_ascii_alphanumeric() || c == '_'),
    ))(i)?;
    Ok((rest, s.to_string()))
}

/// A double-quoted string with `\"`/`\\` escapes (any other `\x` keeps the
/// backslash). Skips leading whitespace.
fn string_lit(input: &str) -> IResult<&str, String> {
    let (input, _) = multispace0(input)?;
    let (body, _) = char('"')(input)?;
    let mut out = String::new();
    let mut it = body.char_indices();
    while let Some((idx, c)) = it.next() {
        match c {
            '\\' => match it.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, other)) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            },
            '"' => return Ok((&body[idx + 1..], out)),
            _ => out.push(c),
        }
    }
    Err(perr(input)) // unterminated
}

/// A list element / name: a quoted string or a bareword.
fn str_or_word(i: &str) -> IResult<&str, String> {
    alt((string_lit, map(word, str::to_string)))(i)
}

/// A parenthesised, comma-separated list of one-or-more `inner` — the shape
/// shared by `(a, b)` tuples, `SHOW(...)`, `ZIP(...)`, `REPORT (...)`, roles, …
fn paren_list1<'a, O, F>(inner: F) -> impl FnMut(&'a str) -> IResult<&'a str, Vec<O>>
where
    F: FnMut(&'a str) -> IResult<&'a str, O>,
{
    delimited(sym('('), separated_list1(sym(','), inner), sym(')'))
}

// ---------------------------------------------------------------------------
// Whitespace / comment trivia between statements
// ---------------------------------------------------------------------------

/// Skip any run of blank space, newlines and whole-line `#` comments — the gaps
/// between statements (header comments are consumed separately).
fn trivia(i: &str) -> IResult<&str, ()> {
    value(
        (),
        many0(alt((
            value((), multispace1),
            value((), pair(char('#'), not_line_ending)),
        ))),
    )(i)
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn parse_headers(i: &str) -> IResult<&str, Header> {
    map(many0(header_line), |lines| Header { lines })(i)
}

/// One leading `# …` line, classified into a `key: value` directive (when the
/// key is an identifier) or a free comment.
fn header_line(i: &str) -> IResult<&str, HeaderLine> {
    let (i, _) = multispace0(i)?;
    let (i, _) = char('#')(i)?;
    let (i, raw) = not_line_ending(i)?;
    let raw = raw.trim();
    let line = match raw.split_once(':') {
        Some((k, v)) if is_ident(k.trim()) => HeaderLine::Directive {
            key: k.trim().to_string(),
            value: v.trim().to_string(),
        },
        _ => HeaderLine::Comment(raw.to_string()),
    };
    Ok((i, line))
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

fn node(i: &str) -> IResult<&str, FlowNode> {
    alt((for_stmt, list_decl, request, report, assign))(i)
}

/// `IDENT = <rest of line>` — value is untokenized (may contain `=`, `&`, …).
fn assign(i: &str) -> IResult<&str, FlowNode> {
    map(
        separated_pair(ident, sym('='), not_line_ending),
        |(key, value)| FlowNode::Assign {
            key,
            value: value.trim().to_string(),
        },
    )(i)
}

fn list_decl(i: &str) -> IResult<&str, FlowNode> {
    map(
        preceded(kw("LIST"), separated_pair(ident, sym('='), producer)),
        |(name, producer)| FlowNode::ListDecl { name, producer },
    )(i)
}

fn request(i: &str) -> IResult<&str, FlowNode> {
    map(preceded(kw("REQUEST"), str_or_word), |name| {
        FlowNode::Request { name }
    })(i)
}

/// `[PARALLEL[(n)]] FOR <pattern> IN (ENVS <clause> | <producer>) … END`.
fn for_stmt(i: &str) -> IResult<&str, FlowNode> {
    let (i, parallel) = opt(parallel_prefix)(i)?;
    let (i, _) = kw("FOR")(i)?;
    let (i, pat) = pattern(i)?;
    let (i, _) = kw("IN")(i)?;

    if let Ok((rest, ())) = kw("ENVS")(i) {
        // ENVS binds a single, non-discard variable.
        let var = match pat.binders.as_slice() {
            [Binder::Named(n)] if !pat.rest => n.clone(),
            _ => return Err(perr(i)),
        };
        let (rest, clause) = env_clause(rest)?;
        let (rest, body) = block_body(rest)?;
        Ok((
            rest,
            FlowNode::ForEnvs {
                var,
                clause,
                body,
                parallel,
            },
        ))
    } else {
        let (i, producer) = producer(i)?;
        let (i, body) = block_body(i)?;
        Ok((
            i,
            FlowNode::ForEach {
                pattern: pat,
                producer,
                body,
                parallel,
            },
        ))
    }
}

/// `PARALLEL` / `PARALLEL(n)` (n ≥ 1).
fn parallel_prefix(i: &str) -> IResult<&str, ParallelSpec> {
    let (i, _) = kw("PARALLEL")(i)?;
    let (i, degree) = opt(delimited(
        sym('('),
        preceded(multispace0, nom::character::complete::u32),
        sym(')'),
    ))(i)?;
    if degree == Some(0) {
        return Err(perr(i));
    }
    Ok((i, ParallelSpec { degree }))
}

/// The statements up to (and consuming) the matching `END`.
fn block_body(i: &str) -> IResult<&str, Vec<FlowNode>> {
    let (i, nodes) = many0(preceded(trivia, node))(i)?;
    let (i, _) = preceded(trivia, kw("END"))(i)?;
    Ok((i, nodes))
}

// ---------------------------------------------------------------------------
// REPORT
// ---------------------------------------------------------------------------

fn report(i: &str) -> IResult<&str, FlowNode> {
    map(
        preceded(
            kw("REPORT"),
            alt((report_request, report_vars, report_computed, report_single)),
        ),
        FlowNode::Report,
    )(i)
}

fn report_request(i: &str) -> IResult<&str, ReportStmt> {
    let (i, _) = kw("REQUEST")(i)?;
    let (i, name) = str_or_word(i)?;
    let (i, alias) = opt(preceded(kw("AS"), str_or_word))(i)?;
    let (i, response_fmt) = opt(preceded(kw("RESPONSE"), resp_fmt))(i)?;
    let (i, show) = map(opt(show_clause), Option::unwrap_or_default)(i)?;
    let (i, with) = map(opt(with_block), Option::unwrap_or_default)(i)?;
    Ok((
        i,
        ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            with,
        },
    ))
}

/// `REPORT (v1, v2, …)`.
fn report_vars(i: &str) -> IResult<&str, ReportStmt> {
    map(paren_list1(ident), ReportStmt::Vars)(i)
}

/// `REPORT "<template>" AS <name>`.
fn report_computed(i: &str) -> IResult<&str, ReportStmt> {
    map(
        pair(string_lit, preceded(kw("AS"), str_or_word)),
        |(template, name)| ReportStmt::Computed { template, name },
    )(i)
}

/// `REPORT <var> [AS <name>]` — a single variable column, optionally renamed.
/// A bareword source (vs. `report_computed`'s quoted string) is what marks this
/// as a *variable* reference rather than a literal template.
fn report_single(i: &str) -> IResult<&str, ReportStmt> {
    let (i, var) = ident(i)?;
    let (i, alias) = opt(preceded(kw("AS"), str_or_word))(i)?;
    Ok((
        i,
        match alias {
            Some(name) => ReportStmt::VarAs { var, name },
            None => ReportStmt::Vars(vec![var]),
        },
    ))
}

fn resp_fmt(i: &str) -> IResult<&str, ResponseFmt> {
    alt((
        value(ResponseFmt::Raw, kw("RAW")),
        value(ResponseFmt::Pretty, kw("PRETTY")),
    ))(i)
}

/// `SHOW(a, b, …)` — at least one field (empty is a parse error).
fn show_clause(i: &str) -> IResult<&str, Vec<String>> {
    preceded(kw("SHOW"), paren_list1(ident))(i)
}

/// `WITH <item>* END`.
fn with_block(i: &str) -> IResult<&str, Vec<WithItem>> {
    let (i, _) = kw("WITH")(i)?;
    let (i, items) = many0(preceded(trivia, with_item))(i)?;
    let (i, _) = preceded(trivia, kw("END"))(i)?;
    Ok((i, items))
}

fn with_item(i: &str) -> IResult<&str, WithItem> {
    alt((
        map(preceded(kw("RESPONSE"), resp_fmt), WithItem::ResponseFmt),
        with_field,
    ))(i)
}

/// `name: <rest of line>` — a full Hurl query (may contain `:` and quotes).
fn with_field(i: &str) -> IResult<&str, WithItem> {
    let (i, name) = ident(i)?;
    let (i, _) = sym(':')(i)?;
    let (i, query) = not_line_ending(i)?;
    let query = query.trim();
    if query.is_empty() {
        return Err(perr(i));
    }
    Ok((
        i,
        WithItem::Field {
            name,
            query: query.to_string(),
        },
    ))
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

fn pattern(i: &str) -> IResult<&str, Pattern> {
    alt((paren_pattern, single_pattern))(i)
}

fn single_pattern(i: &str) -> IResult<&str, Pattern> {
    let (rest, w) = word(i)?;
    match binder(w) {
        Some(b) => Ok((
            rest,
            Pattern {
                binders: vec![b],
                rest: false,
            },
        )),
        None => Err(perr(i)),
    }
}

/// `( binder (',' binder)* [ ',' '...' ] )`.
fn paren_pattern(i: &str) -> IResult<&str, Pattern> {
    let (rest, _) = sym('(')(i)?;
    let (rest, words) = separated_list1(sym(','), word)(rest)?;
    let (rest, _) = sym(')')(rest)?;
    let mut binders = Vec::new();
    let mut rest_pat = false;
    for (idx, w) in words.iter().enumerate() {
        if *w == "..." {
            if idx != words.len() - 1 {
                return Err(perr(i)); // '...' must be last
            }
            rest_pat = true;
        } else if let Some(b) = binder(w) {
            binders.push(b);
        } else {
            return Err(perr(i));
        }
    }
    Ok((
        rest,
        Pattern {
            binders,
            rest: rest_pat,
        },
    ))
}

/// A single pattern position: `_` → discard, else an identifier (`...` is
/// handled by the caller; anything else is rejected).
fn binder(w: &str) -> Option<Binder> {
    if w == "_" {
        Some(Binder::Discard)
    } else if is_ident(w) {
        Some(Binder::Named(w.to_string()))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Producers
// ---------------------------------------------------------------------------

fn producer(i: &str) -> IResult<&str, Producer> {
    alt((
        list_literal,
        files_src,
        folders_src,
        tuples_src,
        zip_src,
        concat_src,
        // A named LIST reference — must not be a reserved producer keyword (so a
        // malformed `FILES …` doesn't fall through to `Named("FILES")`).
        map(
            verify(ident, |s: &str| {
                !matches!(
                    s.to_ascii_uppercase().as_str(),
                    "FILES" | "FOLDERS" | "TUPLES" | "ZIP" | "CONCAT" | "JOIN" | "ENVS"
                )
            }),
            Producer::Named,
        ),
    ))(i)
}

fn files_src(i: &str) -> IResult<&str, Producer> {
    map(
        preceded(
            kw("FILES"),
            pair(string_lit, opt(preceded(kw("MATCH"), string_lit))),
        ),
        |(dir, glob)| Producer::Files { dir, glob },
    )(i)
}

fn folders_src(i: &str) -> IResult<&str, Producer> {
    map(
        preceded(
            kw("FOLDERS"),
            pair(
                string_lit,
                opt(preceded(
                    kw("WITH"),
                    separated_list1(sym(','), separated_pair(ident, sym('='), string_lit)),
                )),
            ),
        ),
        |(dir, roles)| Producer::Folders {
            dir,
            roles: roles.unwrap_or_default(),
        },
    )(i)
}

fn tuples_src(i: &str) -> IResult<&str, Producer> {
    map(
        preceded(pair(kw("TUPLES"), kw("FROM")), string_lit),
        |path| Producer::Tuples { path },
    )(i)
}

fn zip_src(i: &str) -> IResult<&str, Producer> {
    map(preceded(kw("ZIP"), paren_list1(producer)), Producer::Zip)(i)
}

fn concat_src(i: &str) -> IResult<&str, Producer> {
    map(
        preceded(kw("CONCAT"), paren_list1(producer)),
        Producer::Concat,
    )(i)
}

fn list_literal(i: &str) -> IResult<&str, Producer> {
    map(
        delimited(sym('['), separated_list0(sym(','), element), sym(']')),
        Producer::List,
    )(i)
}

fn element(i: &str) -> IResult<&str, Element> {
    alt((
        map(paren_list1(str_or_word), Element::Tuple),
        map(str_or_word, Element::Scalar),
    ))(i)
}

// ---------------------------------------------------------------------------
// ENVS clause
// ---------------------------------------------------------------------------

fn env_clause(i: &str) -> IResult<&str, EnvClause> {
    alt((roles_clause, plain_clause))(i)
}

fn plain_clause(i: &str) -> IResult<&str, EnvClause> {
    map(separated_list1(sym(','), string_lit), EnvClause::Plain)(i)
}

fn roles_clause(i: &str) -> IResult<&str, EnvClause> {
    let (i, roles) = separated_list1(sym(','), role)(i)?;
    let mut baseline = Vec::new();
    let mut comparisons = Vec::new();
    for (is_baseline, mut names) in roles {
        if is_baseline {
            baseline.append(&mut names);
        } else {
            comparisons.append(&mut names);
        }
    }
    Ok((
        i,
        EnvClause::Roles {
            baseline,
            comparisons,
        },
    ))
}

/// One `BASELINE(...)` / `COMPARISON(...)` role; `true` for BASELINE.
fn role(i: &str) -> IResult<&str, (bool, Vec<String>)> {
    pair(
        alt((value(true, kw("BASELINE")), value(false, kw("COMPARISON")))),
        paren_list1(string_lit),
    )(i)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn report_flow(i: &str) -> IResult<&str, ReportFlow> {
    let (i, header) = parse_headers(i)?;
    let (i, nodes) = many0(preceded(trivia, node))(i)?;
    let (i, _) = trivia(i)?;
    Ok((i, ReportFlow { header, nodes }))
}

/// A parse failure, carrying the 1-based line where it occurred so the TUI
/// editor can highlight it.
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

impl std::error::Error for ParseError {}

/// Build a [`ParseError`] whose line number is derived from where `at` (a
/// suffix of `input`) begins. Both nom's error `.input` and any leftover
/// `rest` are always tails of the original slice, so subtracting base
/// pointers yields a byte offset we can turn into a line number.
fn err_at(input: &str, at: &str, message: impl Into<String>) -> ParseError {
    let offset = (at.as_ptr() as usize).saturating_sub(input.as_ptr() as usize);
    let offset = offset.min(input.len());
    let line = input[..offset].bytes().filter(|&b| b == b'\n').count() + 1;
    ParseError {
        line,
        message: message.into(),
    }
}

/// Parse PaperTrail source into a [`ReportFlow`].
pub fn parse_flow(input: &str) -> Result<ReportFlow, ParseError> {
    match report_flow(input) {
        Ok((rest, flow)) if rest.trim().is_empty() => Ok(flow),
        Ok((rest, _)) => {
            let near = rest.lines().next().unwrap_or("").trim();
            Err(err_at(
                input,
                rest,
                format!("unexpected input near: {near}"),
            ))
        }
        Err(nom::Err::Error(e)) | Err(nom::Err::Failure(e)) => {
            Err(err_at(input, e.input, "parse error"))
        }
        Err(nom::Err::Incomplete(_)) => Err(err_at(input, input, "unexpected end of input")),
    }
}

/// Whether `line` opens an `END`-terminated block: a `FOR` loop (with an
/// optional `PARALLEL` / `PARALLEL(n)` marker) or the head of a
/// `REPORT REQUEST … WITH` block. The source editor uses this to auto-indent
/// the following line and to snap a matching `END` back one level.
///
/// It runs the grammar's real token parsers rather than re-recognising the
/// keywords by hand, so it can't drift out of sync with the language. Leading
/// indentation is ignored (the token parsers skip it).
pub fn opens_block(line: &str) -> bool {
    // `[PARALLEL[(n)]] FOR …`
    let for_head = preceded(opt(parallel_prefix), kw("FOR"));
    map(for_head, |_| ())(line).is_ok() || with_block_head(line).is_ok()
}

/// The opener line of a `REPORT REQUEST … WITH … END` block: the statement head
/// up to and including a trailing `WITH` that ends the line (its `END` lands on
/// a later line, so only the head is present here).
fn with_block_head(i: &str) -> IResult<&str, ()> {
    let (i, _) = kw("REPORT")(i)?;
    let (i, _) = kw("REQUEST")(i)?;
    let (i, _) = str_or_word(i)?;
    let (i, _) = opt(preceded(kw("AS"), str_or_word))(i)?;
    let (i, _) = opt(preceded(kw("RESPONSE"), resp_fmt))(i)?;
    let (i, _) = opt(show_clause)(i)?;
    let (i, _) = kw("WITH")(i)?;
    let (i, _) = multispace0(i)?;
    let (i, _) = eof(i)?;
    Ok((i, ()))
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
    fn concat_producer_round_trips() {
        let flow = assert_round_trips(
            "FOR F IN CONCAT(FILES \"a\" MATCH \"*.jpg\", FILES \"b\", FOLDERS \"c\")\n    REQUEST r\nEND\n",
        );
        if let FlowNode::ForEach { producer, .. } = &flow.nodes[0] {
            match producer {
                Producer::Concat(ps) => assert_eq!(ps.len(), 3),
                other => panic!("expected CONCAT, got {other:?}"),
            }
        } else {
            panic!("expected ForEach");
        }
        // Also valid as a named LIST and nestable with ZIP.
        assert_round_trips(
            "LIST ALL = CONCAT(FILES \"x\", ZIP(FILES \"p\", FILES \"q\"))\nFOR ROW IN ALL\n    REQUEST r\nEND\n",
        );
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
    fn report_var_as_renames_a_variable_column() {
        // A bareword source with `AS` is a renamed *variable* column, distinct
        // from the quoted-template computed column. A spaced pretty name must
        // be quoted, and the whole thing round-trips.
        let flow = assert_round_trips("REPORT FILE AS \"Pretty name\"\n");
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::VarAs { var, name }) => {
                assert_eq!(var, "FILE");
                assert_eq!(name, "Pretty name");
            }
            other => panic!("expected VarAs, got {other:?}"),
        }
        // A single-word pretty name needs no quotes.
        assert!(matches!(
            &assert_round_trips("REPORT FILE AS Label\n").nodes[0],
            FlowNode::Report(ReportStmt::VarAs { .. })
        ));
        // A *quoted* source is still a computed (literal) column, not a var.
        assert!(matches!(
            &parse_flow("REPORT \"FILE\" AS Label\n").unwrap().nodes[0],
            FlowNode::Report(ReportStmt::Computed { .. })
        ));
        // A bare `REPORT VAR` (no AS) is unchanged.
        assert!(matches!(
            &parse_flow("REPORT FILE\n").unwrap().nodes[0],
            FlowNode::Report(ReportStmt::Vars(_))
        ));
    }

    #[test]
    fn show_selector_round_trips_and_parses_fields() {
        let flow =
            assert_round_trips("REPORT REQUEST process AS proc RESPONSE RAW SHOW(status, Time)\n");
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::Request { show, .. }) => {
                assert_eq!(show, &vec!["status".to_string(), "Time".to_string()]);
            }
            other => panic!("expected report request, got {other:?}"),
        }
    }

    #[test]
    fn empty_show_selector_is_a_parse_error() {
        assert!(parse_flow("REPORT REQUEST process SHOW()\n").is_err());
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
    fn names_aliases_and_computed_headers_with_punctuation_round_trip() {
        // Names/aliases/headers that contain whitespace or a bareword
        // terminator (`()[],="`) must be re-quoted by the serializer so the
        // save -> to_text -> reload cycle stays stable rather than corrupting.
        assert_round_trips("# collection: c.hurl\nREQUEST \"a,b\"\n");
        assert_round_trips("# collection: c.hurl\nREQUEST \"get(id)\"\n");
        assert_round_trips("# collection: c.hurl\nREPORT REQUEST \"x=y\"\n");
        assert_round_trips(
            "# collection: c.hurl\nREPORT REQUEST proc AS \"Overall Result\" SHOW(status)\n",
        );
        assert_round_trips("# collection: c.hurl\nREPORT \"{{v}}\" AS \"Overall Result\"\n");
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
        assert!(parse_flow("FOR X IN JOIN ON \"k\" (a, b)\n  REQUEST r\nEND\n").is_err());
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
        assert!(parse_flow("PARALLEL(0) FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n").is_err());
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

    #[test]
    fn opens_block_recognises_every_end_terminated_opener() {
        // FOR loops, with and without a PARALLEL[(n)] marker, and indented.
        assert!(opens_block("FOR F IN FILES \"docs\""));
        assert!(opens_block("    FOR (a, b) IN ZIP(x, y)"));
        assert!(opens_block("PARALLEL FOR F IN FILES \"docs\""));
        assert!(opens_block("PARALLEL(4) FOR F IN FILES \"docs\""));
        // REPORT REQUEST … WITH heads, including optional clauses before WITH.
        assert!(opens_block("REPORT REQUEST process_file WITH"));
        assert!(opens_block(
            "REPORT REQUEST r AS a RESPONSE RAW SHOW(x, y) WITH"
        ));
        assert!(opens_block("    REPORT REQUEST r WITH   "));
    }

    #[test]
    fn opens_block_rejects_non_openers() {
        assert!(!opens_block("REQUEST oauth"));
        assert!(!opens_block("REPORT REQUEST process_file")); // no WITH → single line
        assert!(!opens_block("REPORT REQUEST r SHOW(a)")); // WITH not trailing
        assert!(!opens_block("END"));
        assert!(!opens_block("FORMAT = json")); // FOR must be a whole keyword
        assert!(!opens_block("PARALLEL")); // needs a FOR to open a block
        assert!(!opens_block("field: jsonpath \"$.id\" WITH")); // WITH mid-query, not a REPORT
    }
}
