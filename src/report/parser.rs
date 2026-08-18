//! PaperTrail source text → [`ReportFlow`] AST, via a `nom` combinator
//! grammar. The full grammar is documented in the block comment below.

use nom::{
    IResult,
    branch::alt,
    bytes::complete::{take_while, take_while1},
    character::complete::{char, multispace0, multispace1, not_line_ending, satisfy},
    combinator::{eof, map, opt, peek, recognize, value, verify},
    multi::{many0, separated_list0, separated_list1},
    sequence::{delimited, pair, preceded, separated_pair, tuple},
};

use crate::report::flow::{
    Binder, Element, EnvClause, FlowNode, Header, HeaderLine, ImageSpec, ParallelSpec, ParamDecl,
    ParamKind, Pattern, Producer, ReportFlow, ReportStmt, ResponseFmt, RoleBinding, RoleRef,
    ShowField, WithItem,
};
use crate::report::model::{ColumnClauses, StatKind};

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
              | param-decl                             # prelude only
              | request
              | report
              | for-each
              | for-envs
              | end

assign       := IDENT '=' value                       # incl. PRELUDE_* settings
list-decl    := 'LIST' IDENT '=' producer
param-decl   := 'PARAM' [ param-kind ] IDENT [ '=' value ] [ 'LABEL' name ]
param-kind   := 'TEXT' | 'NUMBER' | 'ENV' | 'FOLDER' | 'FILE'
              | 'CHOICE' '(' [ name (',' name)* ] ')'

request      := 'REQUEST' name
report       := 'REPORT' report-target
report-target:= 'REQUEST' name [ 'AS' name ] [ response-fmt ] [ show ] [ hide ] [ with-block ]
              | IDENT 'AS' name                          # renamed variable column
              | var-list
              | string 'AS' name                         # computed column
var-list     := IDENT | '(' IDENT (',' IDENT)* ')'
response-fmt := 'RESPONSE' ('RAW' | 'PRETTY')
show         := 'SHOW' '(' IDENT (',' IDENT)* ')'
hide         := 'HIDE' '(' IDENT (',' IDENT)* ')'
with-block   := 'WITH' with-item* 'END'
with-item    := response-fmt | field-def
field-def    := name ':' hurl-query                    # full Hurl query + filters
                                                       # quote a multi-word name

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
///
/// Exposed to `edit` so an inline field that renames a loop variable can refuse
/// a name the parser would then reject, rather than writing text that no longer
/// round-trips through `parse_flow`.
pub(crate) fn is_ident(s: &str) -> bool {
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

/// A whole-line `#` comment kept as a node, so that commenting a block out in
/// the source doesn't destroy it the next time the editors re-serialize the
/// flow. The text is stored exactly as written after the `#`.
fn comment_node(i: &str) -> IResult<&str, FlowNode> {
    map(preceded(char('#'), not_line_ending), |text: &str| {
        FlowNode::Comment(text.to_string())
    })(i)
}

/// A statement or a comment. Bodies parse with this rather than `node` so a
/// comment keeps its position among the statements around it.
fn node_or_comment(i: &str) -> IResult<&str, FlowNode> {
    alt((comment_node, node))(i)
}

/// Skip any run of blank space, newlines and whole-line `#` comments. Only used
/// where a comment has nowhere to live (by `opens_block`, which is a lookahead
/// and keeps nothing); statement bodies use `multispace0` + `node_or_comment`,
/// and `WITH` blocks `multispace0` + `with_item_or_comment`.
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
    let (i, verbatim) = not_line_ending(i)?;
    let raw = verbatim.trim();
    let line = match raw.split_once(':') {
        Some((k, v)) if is_ident(k.trim()) => HeaderLine::Directive {
            key: k.trim().to_string(),
            value: v.trim().to_string(),
        },
        // Kept exactly as typed, unlike a directive: a block commented out
        // above the first statement lands here, and re-spacing it would
        // quietly destroy its indentation.
        _ => HeaderLine::Comment(verbatim.to_string()),
    };
    Ok((i, line))
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

fn node(i: &str) -> IResult<&str, FlowNode> {
    alt((for_stmt, list_decl, param_decl, request, report, assign))(i)
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

/// `PARAM [<kind>] IDENT [ '=' value ] [ 'LABEL' text ]`.
///
/// The kind is optional (omitted = `TEXT`), which needs a lookahead to stay
/// unambiguous: `PARAM TEXT = "x"` declares a parameter *named* `TEXT`, while
/// `PARAM TEXT NAME = "x"` is a text parameter named `NAME`. So a leading kind
/// word is only consumed when an identifier follows it — otherwise the word is
/// the parameter's own name.
fn param_decl(i: &str) -> IResult<&str, FlowNode> {
    let (i, _) = kw("PARAM")(i)?;
    // `peek(ident)` after the kind is what disambiguates: without a name
    // following, the word we just read was the name itself.
    let (i, kind) = opt(map(pair(param_kind, peek(ident)), |(k, _)| k))(i)?;
    let kind = kind.unwrap_or_default();
    let (i, name) = ident(i)?;
    let (i, default) = opt(preceded(sym('='), str_or_word))(i)?;
    let (i, label) = opt(preceded(kw("LABEL"), str_or_word))(i)?;
    Ok((
        i,
        FlowNode::Param(ParamDecl {
            kind,
            name,
            default,
            label,
        }),
    ))
}

/// `TEXT | NUMBER | ENV | FOLDER | FILE | CHOICE(a, b, …)`.
fn param_kind(i: &str) -> IResult<&str, ParamKind> {
    alt((
        map(
            preceded(kw("CHOICE"), paren_list1(str_or_word)),
            |options| ParamKind::Choice(options),
        ),
        // An empty option list is a grammatical `CHOICE` with nothing to
        // choose from; accepting it here lets validation say so in words
        // instead of the parser rejecting the whole line as unknown syntax.
        map(preceded(kw("CHOICE"), pair(sym('('), sym(')'))), |_| {
            ParamKind::Choice(Vec::new())
        }),
        value(ParamKind::Text, kw("TEXT")),
        value(ParamKind::Number, kw("NUMBER")),
        value(ParamKind::Env, kw("ENV")),
        value(ParamKind::Folder, kw("FOLDER")),
        value(ParamKind::File, kw("FILE")),
    ))(i)
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
    let (i, nodes) = many0(preceded(multispace0, node_or_comment))(i)?;
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
    let (i, hide) = map(opt(hide_clause), Option::unwrap_or_default)(i)?;
    let (i, with) = map(opt(with_block), Option::unwrap_or_default)(i)?;
    Ok((
        i,
        ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            hide,
            with,
        },
    ))
}

/// `REPORT (v1, v2, …)`.
fn report_vars(i: &str) -> IResult<&str, ReportStmt> {
    map(paren_list1(ident), ReportStmt::Vars)(i)
}

/// `REPORT "<template>" AS <name> [STATISTICS(…)] [IMAGE[(…)]]`.
fn report_computed(i: &str) -> IResult<&str, ReportStmt> {
    let (i, template) = string_lit(i)?;
    let (i, name) = preceded(kw("AS"), str_or_word)(i)?;
    let (i, clauses) = column_clauses(i)?;
    Ok((
        i,
        ReportStmt::Computed {
            template,
            name,
            stats: clauses.stats,
            image: clauses.image,
            truth: clauses.truth,
            detail: clauses.detail,
        },
    ))
}

/// `REPORT <var> [AS <name>] [STATISTICS(…)]` — a single variable column,
/// optionally renamed and/or summarised. A bareword source (vs.
/// `report_computed`'s quoted string) is what marks this as a *variable*
/// reference rather than a literal template. `STATISTICS(…)` without an explicit
/// `AS` uses the variable name as the column header.
fn report_single(i: &str) -> IResult<&str, ReportStmt> {
    let (i, var) = ident(i)?;
    let (i, alias) = opt(preceded(kw("AS"), str_or_word))(i)?;
    let (
        i,
        ColumnClauses {
            stats,
            image,
            truth,
            detail,
        },
    ) = column_clauses(i)?;
    Ok((
        i,
        match (
            alias,
            stats.is_empty() && image.is_none() && truth.is_none() && !detail,
        ) {
            (Some(name), _) => ReportStmt::VarAs {
                var,
                name,
                stats,
                image,
                truth,
                detail,
            },
            // A bare `REPORT X` is a plain variable column, but one carrying a
            // clause needs a named column to hang the clause on, so the
            // variable name becomes the header.
            (None, false) => {
                let name = var.clone();
                ReportStmt::VarAs {
                    var,
                    name,
                    stats,
                    image,
                    truth,
                    detail,
                }
            }
            (None, true) => ReportStmt::Vars(vec![var]),
        },
    ))
}

/// The optional trailing column clauses -- `STATISTICS(…)`, `IMAGE[(…)]` and
/// `TRUTH "…"` -- in any order, since none is more natural than another.
fn column_clauses(i: &str) -> IResult<&str, ColumnClauses> {
    let mut out = ColumnClauses::default();
    let mut rest = i;
    loop {
        if let Ok((r, s)) = statistics_clause(rest) {
            out.stats = s;
            rest = r;
            continue;
        }
        if out.image.is_none()
            && let Ok((r, im)) = image_clause(rest)
        {
            out.image = Some(im);
            rest = r;
            continue;
        }
        if out.truth.is_none()
            && let Ok((r, t)) = truth_clause(rest)
        {
            out.truth = Some(t);
            rest = r;
            continue;
        }
        if !out.detail
            && let Ok((r, _)) = detail_clause(rest)
        {
            out.detail = true;
            rest = r;
            continue;
        }
        return Ok((rest, out));
    }
}

/// `DETAIL` -- the placement flag that moves a column out of the table and into
/// its row's drill-down. A bare keyword: it says *where* the column goes, and
/// there is only one other place for it to be.
fn detail_clause(i: &str) -> IResult<&str, ()> {
    let (i, _) = kw("DETAIL")(i)?;
    Ok((i, ()))
}

/// `TRUTH "<template>"` -- the column's expected value, interpolated per row.
/// The argument is a mandatory string literal (see
/// [`crate::report::model::split_truth`] for why it may not be bare).
fn truth_clause(i: &str) -> IResult<&str, String> {
    preceded(kw("TRUTH"), string_lit)(i)
}

/// `IMAGE` / `IMAGE(HEIGHT n | WIDTH n | FIT, …)` -- the render hint that makes
/// a column's value be drawn as a picture by writers that can show one.
fn image_clause(i: &str) -> IResult<&str, ImageSpec> {
    let (i, _) = kw("IMAGE")(i)?;
    let (i, opts) = opt(paren_list1(image_opt))(i)?;
    let mut spec = ImageSpec::default();
    for opt in opts.into_iter().flatten() {
        match opt {
            ImageOpt::Height(n) => spec.height = Some(n),
            ImageOpt::Width(n) => spec.width = Some(n),
            ImageOpt::Fit => spec.fit = true,
        }
    }
    Ok((i, spec))
}

#[derive(Clone, Copy)]
enum ImageOpt {
    Height(u32),
    Width(u32),
    Fit,
}

fn image_opt(i: &str) -> IResult<&str, ImageOpt> {
    let px = preceded(multispace0, nom::character::complete::u32);
    alt((
        value(ImageOpt::Fit, kw("FIT")),
        map(
            preceded(kw("HEIGHT"), verify(px, |n: &u32| *n > 0)),
            ImageOpt::Height,
        ),
        map(
            preceded(
                kw("WIDTH"),
                verify(
                    preceded(multispace0, nom::character::complete::u32),
                    |n: &u32| *n > 0,
                ),
            ),
            ImageOpt::Width,
        ),
    ))(i)
}

/// `STATISTICS(stat, …)` — the summary-statistics clause on a `REPORT … AS …`.
fn statistics_clause(i: &str) -> IResult<&str, Vec<StatKind>> {
    preceded(kw("STATISTICS"), paren_list1(stat_kind))(i)
}

/// One statistic keyword inside a `STATISTICS(…)` clause.
fn stat_kind(i: &str) -> IResult<&str, StatKind> {
    let (rest, w) = str_or_word(i)?;
    match StatKind::parse(&w) {
        Some(k) => Ok((rest, k)),
        None => Err(perr(i)),
    }
}

fn resp_fmt(i: &str) -> IResult<&str, ResponseFmt> {
    alt((
        value(ResponseFmt::Raw, kw("RAW")),
        value(ResponseFmt::Pretty, kw("PRETTY")),
    ))(i)
}

/// `SHOW(a, b STATISTICS(MEAN), …)` — at least one field (empty is a parse
/// error). Each field may carry its own `STATISTICS(…)`, which summarises the
/// column that field produces.
fn show_clause(i: &str) -> IResult<&str, Vec<ShowField>> {
    preceded(kw("SHOW"), paren_list1(show_field))(i)
}

/// One `SHOW(…)` field: a name and an optional `STATISTICS(…)` clause.
fn show_field(i: &str) -> IResult<&str, ShowField> {
    let (i, field) = ident(i)?;
    let (i, stats) = opt(statistics_clause)(i)?;
    Ok((
        i,
        ShowField {
            field,
            stats: stats.unwrap_or_default(),
        },
    ))
}

/// `HIDE(a, b, …)` — at least one field (empty is a parse error).
fn hide_clause(i: &str) -> IResult<&str, Vec<String>> {
    preceded(kw("HIDE"), paren_list1(ident))(i)
}

/// `WITH <item>* END`.
///
/// Items are parsed with `multispace0` + [`with_item_or_comment`] rather than
/// `trivia`, so a comment inside the block keeps its place among the fields
/// instead of being skipped as whitespace — commenting a field out and then
/// editing the request elsewhere must not delete the commented line.
fn with_block(i: &str) -> IResult<&str, Vec<WithItem>> {
    let (i, _) = kw("WITH")(i)?;
    let (i, items) = many0(preceded(multispace0, with_item_or_comment))(i)?;
    let (i, _) = preceded(multispace0, kw("END"))(i)?;
    Ok((i, items))
}

/// A `WITH` item or a whole-line comment between items. Comments are tried
/// first: a `#` can't start any real item, and a field would otherwise swallow
/// the line as a name.
fn with_item_or_comment(i: &str) -> IResult<&str, WithItem> {
    alt((with_comment, with_item))(i)
}

fn with_comment(i: &str) -> IResult<&str, WithItem> {
    map(preceded(char('#'), not_line_ending), |text: &str| {
        WithItem::Comment(text.to_string())
    })(i)
}

fn with_item(i: &str) -> IResult<&str, WithItem> {
    alt((
        map(preceded(kw("RESPONSE"), resp_fmt), WithItem::ResponseFmt),
        with_field,
    ))(i)
}

/// A `WITH` field name: a quoted string (for multi-word / spaced names like
/// `"Response Time"`) or a bareword identifier. Unlike a general `word`, the
/// bareword form stops at the `:` separator.
fn with_field_name(i: &str) -> IResult<&str, String> {
    alt((string_lit, ident))(i)
}

/// `name: <rest of line> [STATISTICS(…)] [IMAGE[(…)]]` — a full Hurl query (may
/// contain `:` and quotes) or an intrinsic name, with optional trailing
/// statistics and image clauses. `name` may be quoted to allow spaces.
fn with_field(i: &str) -> IResult<&str, WithItem> {
    let (i, name) = with_field_name(i)?;
    let (i, _) = sym(':')(i)?;
    let (i, rest) = not_line_ending(i)?;
    // Peel the optional trailing `STATISTICS(…)`/`IMAGE(…)` clauses off the
    // query text (the same whole-word, outside-quotes rule the `columns:`
    // directive uses), leaving the bare query. It has to be done textually
    // here rather than with a combinator because the query itself runs to the
    // end of the line and may contain almost anything.
    let (query, clauses) = crate::report::model::split_column_clauses(rest);
    let query = query.trim();
    if query.is_empty() {
        return Err(perr(i));
    }
    Ok((
        i,
        WithItem::Field {
            name,
            query: query.to_string(),
            stats: clauses.stats,
            image: clauses.image,
            truth: clauses.truth,
            detail: clauses.detail,
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
            tuple((
                string_lit,
                opt(preceded(kw("MATCH"), string_lit)),
                opt(preceded(
                    kw("WITH"),
                    separated_list1(sym(','), role_binding),
                )),
            )),
        ),
        |(dir, glob, roles)| Producer::Folders {
            dir,
            glob,
            roles: roles.unwrap_or_default(),
        },
    )(i)
}

/// One `role="glob"` binding, with an optional trailing `?` marking the role
/// optional (it may match no file, binding empty, rather than failing the run).
fn role_binding(i: &str) -> IResult<&str, RoleBinding> {
    map(
        pair(separated_pair(ident, sym('='), string_lit), opt(sym('?'))),
        |((name, glob), mark)| RoleBinding {
            name,
            glob,
            optional: mark.is_some(),
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
    let mut baseline_show = Vec::new();
    for (is_baseline, mut refs, mut show) in roles {
        if is_baseline {
            baseline.append(&mut refs);
            baseline_show.append(&mut show);
        } else {
            comparisons.append(&mut refs);
        }
    }
    Ok((
        i,
        EnvClause::Roles {
            baseline,
            comparisons,
            baseline_show,
        },
    ))
}

/// One `BASELINE(...) [SHOW(...)]` / `COMPARISON(...)` role.
///
/// Returns `(is_baseline, refs, show_fields)`.  Each ref is a [`RoleRef`] — a
/// live env name or a `FILE("…")` snapshot.  `SHOW` after `COMPARISON` is a
/// hard parse error (returned as `nom::Err::Failure`) so it can't be silently
/// swallowed by the surrounding `alt`.
fn role(i: &str) -> IResult<&str, (bool, Vec<RoleRef>, Vec<ShowField>)> {
    let (i, is_baseline) = alt((value(true, kw("BASELINE")), value(false, kw("COMPARISON"))))(i)?;
    let (i, refs) = paren_list1(role_ref)(i)?;
    // SHOW(…) is only legal on a BASELINE role.  If we see SHOW after a
    // COMPARISON, surface it as a Failure (not a soft Error) so the user gets a
    // clear rejection rather than a silent parse-stop.
    let (i, show) = if is_baseline {
        let (i, maybe) = opt(show_clause)(i)?;
        (i, maybe.unwrap_or_default())
    } else {
        if peek(show_clause)(i).is_ok() {
            return Err(nom::Err::Failure(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        (i, Vec::new())
    };
    Ok((i, (is_baseline, refs, show)))
}

/// A single role argument: a `FILE("…")` snapshot reference, or a bare quoted
/// environment name.  `FILE` is matched first (a bare env name can't start with
/// `FILE(` — it is a quoted string), and only ever here in argument position,
/// so it never collides with the `FILE` loop-variable name in `FOR FILE IN
/// FILES`.
fn role_ref(i: &str) -> IResult<&str, RoleRef> {
    alt((map(file_ref, RoleRef::File), map(string_lit, RoleRef::Env)))(i)
}

/// `FILE("path")` — the snapshot path inside a role argument.
fn file_ref(i: &str) -> IResult<&str, String> {
    let (i, _) = kw("FILE")(i)?;
    delimited(sym('('), string_lit, sym(')'))(i)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn report_flow(i: &str) -> IResult<&str, ReportFlow> {
    let (i, header) = parse_headers(i)?;
    let (i, nodes) = many0(preceded(multispace0, node_or_comment))(i)?;
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
    let (i, _) = opt(hide_clause)(i)?;
    let (i, _) = kw("WITH")(i)?;
    let (i, _) = multispace0(i)?;
    let (i, _) = eof(i)?;
    Ok((i, ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug that motivated `FlowNode::Comment`: a block commented out in the
    /// body used to vanish, because `trivia` threw comments away and the
    /// editors re-serialize the flow after every edit.
    #[test]
    fn a_commented_out_block_survives_a_round_trip() {
        let src =
            "# collection: c\n\nREQUEST b\n# FOR X IN FILES \"*.txt\"\n#     REQUEST a\n# END\n";
        let flow = parse_flow(src).expect("parses");
        assert_eq!(flow.to_text(), src, "round-trips byte for byte");
    }

    /// Whatever follows the `#` is kept verbatim — a comment is not re-spaced,
    /// so indentation inside a commented-out block is preserved.
    #[test]
    fn comment_text_is_kept_exactly_as_written() {
        let src = "# collection: c\n\nREQUEST b\n#no space\n#      wide   gap\n";
        let flow = parse_flow(src).expect("parses");
        assert_eq!(flow.to_text(), src);
    }

    /// A comment sits where it was written, not hoisted to the top or sunk to
    /// the bottom of its block.
    #[test]
    fn a_comment_keeps_its_position_in_the_body() {
        let flow =
            parse_flow("# collection: c\n\nREQUEST a\n# between\nREQUEST b\n").expect("parses");
        let kinds: Vec<_> = flow
            .nodes
            .iter()
            .map(|n| matches!(n, FlowNode::Comment(_)))
            .collect();
        assert_eq!(kinds, vec![false, true, false]);
    }

    /// Comments above the first statement belong to the *header* — that is what
    /// makes `# collection:` a directive, and it holds even across a blank line
    /// so a directive written below one isn't silently demoted to a comment.
    #[test]
    fn leading_comments_still_belong_to_the_header() {
        let flow = parse_flow("# collection: c\n# a note\nREQUEST b\n").expect("parses");
        assert!(
            !flow.nodes.iter().any(|n| matches!(n, FlowNode::Comment(_))),
            "the note stayed in the header"
        );
    }

    /// …and header comments are kept verbatim too, so a block commented out up
    /// there survives with its indentation, it just sits with the directives.
    #[test]
    fn a_header_comment_keeps_its_spacing() {
        let flow = parse_flow("# collection: c\n#     indented note\nREQUEST b\n").expect("parses");
        assert!(
            flow.to_text().contains("#     indented note"),
            "{:?}",
            flow.to_text()
        );
    }
    /// The same gap, one level down: a `WITH` block is the one place a comment
    /// still had nowhere to live, so commenting a column out and then touching
    /// the request from an editor deleted the line. The `#` is re-indented to
    /// the block's own depth on the way out — the *text* after it is verbatim.
    #[test]
    fn a_commented_out_with_field_survives_a_round_trip() {
        let src = concat!(
            "# collection: c\n\n",
            "REPORT REQUEST face AS f WITH\n",
            "    Score: jsonpath \"$.score\"\n",
            "    #    Frame: jsonpath \"$.frame\" IMAGE(HEIGHT 110)\n",
            "    Verdict: jsonpath \"$.verdict\"\n",
            "END\n",
        );
        let flow = parse_flow(src).expect("parses");
        assert_eq!(flow.to_text(), src, "round-trips byte for byte");
    }

    /// A comment keeps its place among the fields rather than being hoisted or
    /// sunk, which is the whole point: `# Frame:` above `Verdict:` means the
    /// column that *was* there, not a note about the block.
    #[test]
    fn a_with_comment_keeps_its_position_among_the_fields() {
        let flow = parse_flow(concat!(
            "# collection: c\n\n",
            "REPORT REQUEST face AS f WITH\n",
            "    a: HttpStatus\n",
            "#    b: Time\n",
            "    c: Response\n",
            "END\n",
        ))
        .expect("parses");
        let Some(FlowNode::Report(ReportStmt::Request { with, .. })) = flow.nodes.first() else {
            panic!("expected a report request, got {:?}", flow.nodes);
        };
        let kinds: Vec<_> = with
            .iter()
            .map(|w| matches!(w, WithItem::Comment(_)))
            .collect();
        assert_eq!(kinds, vec![false, true, false]);
    }

    /// A comment on the last line of the block still belongs to it, rather than
    /// being swallowed by whatever skips whitespace before `END`.
    #[test]
    fn a_with_comment_just_above_end_is_kept() {
        let src = concat!(
            "# collection: c\n\n",
            "REPORT REQUEST face AS f WITH\n",
            "    a: HttpStatus\n",
            "    #    b: Time\n",
            "END\n",
        );
        let flow = parse_flow(src).expect("parses");
        assert_eq!(flow.to_text(), src);
    }

    use crate::report::flow::{
        Binder, Element, EnvClause, FlowNode, ParallelSpec, Producer, ReportStmt, RoleRef, WithItem,
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
        assert_eq!(flow.header.get("name"), Some("Smoke"));
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
    fn a_parameter_declares_a_type_a_default_and_a_prompt() {
        let flow = assert_round_trips(
            "PARAM CHOICE(\"v4.2\", \"v4.3\") VERSION = \"v4.3\" LABEL \"API version\"\nREPORT REQUEST r\n",
        );
        match &flow.nodes[0] {
            FlowNode::Param(p) => {
                assert_eq!(
                    p.kind,
                    ParamKind::Choice(vec!["v4.2".into(), "v4.3".into()])
                );
                assert_eq!(p.name, "VERSION");
                assert_eq!(p.default.as_deref(), Some("v4.3"));
                assert_eq!(p.label.as_deref(), Some("API version"));
            }
            other => panic!("expected Param, got {other:?}"),
        }
    }

    #[test]
    fn every_part_of_a_parameter_but_its_name_is_optional() {
        let flow = parse_flow("PARAM TICKETS\nREPORT REQUEST r\n").unwrap();
        match &flow.nodes[0] {
            FlowNode::Param(p) => {
                // No type written means free text, and no default means the
                // parameter is required rather than empty-by-default.
                assert_eq!(p.kind, ParamKind::Text);
                assert_eq!(p.name, "TICKETS");
                assert_eq!(p.default, None);
                assert_eq!(p.label, None);
            }
            other => panic!("expected Param, got {other:?}"),
        }
        // The canonical form spells the type out, so nobody has to know which
        // one they got by saying nothing.
        assert!(flow.to_text().starts_with("PARAM TEXT TICKETS\n"));
    }

    /// The type is optional, so a parameter *named* after a type keyword has to
    /// resolve the other way — the word is only a type when a name follows it.
    #[test]
    fn a_parameter_named_after_a_type_is_still_its_own_name() {
        let flow = parse_flow("PARAM ENV = \"staging\"\n").unwrap();
        match &flow.nodes[0] {
            FlowNode::Param(p) => {
                assert_eq!(p.name, "ENV");
                assert_eq!(p.kind, ParamKind::Text);
                assert_eq!(p.default.as_deref(), Some("staging"));
            }
            other => panic!("expected Param, got {other:?}"),
        }
        let typed = parse_flow("PARAM ENV TARGET = \"staging\"\n").unwrap();
        match &typed.nodes[0] {
            FlowNode::Param(p) => {
                assert_eq!(p.name, "TARGET");
                assert_eq!(p.kind, ParamKind::Env);
            }
            other => panic!("expected Param, got {other:?}"),
        }
    }

    /// Most parameters will never carry a `LABEL`, and the people who only run
    /// reports shouldn't be shouted at in identifiers.
    #[test]
    fn a_parameter_without_a_label_still_has_a_readable_prompt() {
        let cases = [
            ("PARAM TICKET_REF", "Ticket ref"),
            ("PARAM TEXT api_version", "Api version"),
            ("PARAM FOLDER IMAGES", "Images"),
            // Already mixed-case: deliberate, so left exactly as written.
            ("PARAM TEXT imageWidth", "imageWidth"),
            ("PARAM TEXT iOS_build", "iOS build"),
        ];
        for (src, want) in cases {
            let flow = parse_flow(&format!("{src}\n")).unwrap();
            assert_eq!(flow.params()[0].prompt(), want, "for {src}");
        }
        // A written LABEL always wins.
        let flow = parse_flow("PARAM TICKET_REF LABEL \"Ticket\"\n").unwrap();
        assert_eq!(flow.params()[0].prompt(), "Ticket");
    }

    #[test]
    fn the_declared_parameters_are_readable_without_running_anything() {
        let flow = parse_flow(
            "# collection: c\nPARAM ENV TARGET = \"staging\"\nPARAM FOLDER IMAGES = \"./t\"\nREPORT REQUEST r\n",
        )
        .unwrap();
        let names: Vec<&str> = flow.params().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["TARGET", "IMAGES"]);
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
    fn folders_match_glob_and_optional_roles_round_trip() {
        let flow = assert_round_trips(
            "FOR CASE IN FOLDERS \"cases\" MATCH \"**/case_*\" WITH front=\"*_front.*\", back=\"*_back.*\"?\n    REQUEST r\nEND\n",
        );
        if let FlowNode::ForEach { producer, .. } = &flow.nodes[0] {
            match producer {
                Producer::Folders { glob, roles, .. } => {
                    assert_eq!(glob.as_deref(), Some("**/case_*"));
                    assert!(!roles[0].optional);
                    assert!(roles[1].optional, "trailing ? marks the role optional");
                }
                other => panic!("expected FOLDERS, got {other:?}"),
            }
        } else {
            panic!("expected ForEach");
        }
        // MATCH without roles, and roles without MATCH, are both still valid.
        assert_round_trips("FOR D IN FOLDERS \"cases\" MATCH \"**\"\n    REQUEST r\nEND\n");
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
                    baseline: vec![RoleRef::Env("prod-au".into())],
                    comparisons: vec![
                        RoleRef::Env("staging-au".into()),
                        RoleRef::Env("staging-eu".into()),
                    ],
                    baseline_show: vec![],
                }
            );
        } else {
            panic!("expected ForEnvs roles");
        }
    }

    #[test]
    fn envs_roles_accept_file_snapshots() {
        // FILE("…") is accepted in both BASELINE and COMPARISON argument
        // positions, parsed as a RoleRef::File, and round-trips through the
        // serializer.
        let flow = assert_round_trips(
            "FOR TARGET IN ENVS BASELINE(FILE(\"prod.baseline\")), COMPARISON(\"staging\", FILE(\"old.baseline\"))\n    REQUEST r\nEND\n",
        );
        if let FlowNode::ForEnvs { clause, .. } = &flow.nodes[0] {
            assert_eq!(
                clause,
                &EnvClause::Roles {
                    baseline: vec![RoleRef::File("prod.baseline".into())],
                    comparisons: vec![
                        RoleRef::Env("staging".into()),
                        RoleRef::File("old.baseline".into()),
                    ],
                    baseline_show: vec![],
                }
            );
        } else {
            panic!("expected ForEnvs roles with FILE");
        }
    }

    #[test]
    fn file_snapshot_role_does_not_shadow_the_file_loop_var() {
        // `FILE` as a loop variable (`FOR FILE IN FILES`) is an identifier, not
        // the argument-position `FILE(…)` keyword, so both parse in one flow.
        assert_round_trips(
            "FOR FILE IN FILES \"docs\"\n    FOR TARGET IN ENVS BASELINE(FILE(\"b.baseline\")), COMPARISON(\"staging\")\n        REQUEST r\n    END\nEND\n",
        );
    }

    #[test]
    fn with_field_supports_quoted_names_and_statistics() {
        // A multi-word quoted field name, an intrinsic-name query (`Time`), and
        // a trailing `STATISTICS(…)` clause all parse and round-trip.
        let flow = assert_round_trips(
            "REPORT REQUEST analyze AS proc WITH\n    \"Response Time\": Time STATISTICS(MEAN, MEDIAN)\n    Status: HttpStatus\nEND\n",
        );
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::Request { with, .. }) => {
                match &with[0] {
                    WithItem::Field {
                        name, query, stats, ..
                    } => {
                        assert_eq!(name, "Response Time");
                        assert_eq!(query, "Time");
                        assert_eq!(stats, &vec![StatKind::Mean, StatKind::Median]);
                    }
                    other => panic!("expected field, got {other:?}"),
                }
                // The plain field keeps an empty stats vec.
                assert!(matches!(&with[1], WithItem::Field { stats, .. } if stats.is_empty()));
            }
            other => panic!("expected REPORT REQUEST, got {other:?}"),
        }
        // The stats attach to the field's output column (`alias.field`).
        let cs = flow.column_stats();
        assert_eq!(
            cs.get("proc.Response Time"),
            Some(&vec![StatKind::Mean, StatKind::Median])
        );
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
            FlowNode::Report(ReportStmt::VarAs { var, name, .. }) => {
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

    /// The sources the grammar must reject. Grouped into one table: each was a
    /// test whose whole body was the same "this does not parse" assertion, and
    /// the edges of the grammar are far easier to read against each other than
    /// scattered down the file one `#[test]` at a time.
    #[test]
    fn malformed_sources_are_parse_errors() {
        for (why, src) in [
            ("SHOW with no fields", "REPORT REQUEST process SHOW()\n"),
            ("HIDE with no fields", "REPORT REQUEST process HIDE()\n"),
            ("FOR without its END", "FOR X IN FILES \"d\"\n  REQUEST r\n"),
            ("END without a FOR", "REQUEST r\nEND\n"),
            (
                "the reserved JOIN source",
                "FOR X IN JOIN ON \"k\" (a, b)\n  REQUEST r\nEND\n",
            ),
            (
                "an unterminated string",
                "FOR X IN FILES \"oops\n  REQUEST r\nEND\n",
            ),
            (
                "PARALLEL with zero workers",
                "PARALLEL(0) FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n",
            ),
            (
                "PARALLEL with a non-numeric degree",
                "PARALLEL(lots) FOR FILE IN FILES \"docs\"\n  REQUEST r\nEND\n",
            ),
            (
                "PARALLEL on something that isn't a FOR",
                "PARALLEL REQUEST r\n",
            ),
            (
                "SHOW after COMPARISON",
                "FOR T IN ENVS BASELINE(\"p\"), COMPARISON(\"s\") SHOW(Time)\n    REQUEST r\nEND\n",
            ),
        ] {
            assert!(parse_flow(src).is_err(), "{why} must not parse: {src:?}");
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
        // HIDE clause between SHOW and WITH must not confuse the opener detector.
        assert!(opens_block(
            "REPORT REQUEST r AS a RESPONSE RAW SHOW(x) HIDE(b) WITH"
        ));
        assert!(opens_block("REPORT REQUEST r HIDE(Response) WITH"));
    }

    #[test]
    fn opens_block_rejects_non_openers() {
        assert!(!opens_block("REQUEST oauth"));
        assert!(!opens_block("REPORT REQUEST process_file")); // no WITH → single line
        assert!(!opens_block("REPORT REQUEST r SHOW(a)")); // WITH not trailing
        assert!(!opens_block("REPORT REQUEST r HIDE(a)")); // no WITH → single line
        assert!(!opens_block("END"));
        assert!(!opens_block("FORMAT = json")); // FOR must be a whole keyword
        assert!(!opens_block("PARALLEL")); // needs a FOR to open a block
        assert!(!opens_block("field: jsonpath \"$.id\" WITH")); // WITH mid-query, not a REPORT
    }

    #[test]
    fn hide_clause_round_trips_and_parses_fields() {
        let flow = assert_round_trips(
            "REPORT REQUEST process AS proc RESPONSE RAW SHOW(status) HIDE(Response, Time)\n",
        );
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::Request { show, hide, .. }) => {
                assert_eq!(show, &vec!["status".to_string()]);
                assert_eq!(hide, &vec!["Response".to_string(), "Time".to_string()]);
            }
            other => panic!("expected report request, got {other:?}"),
        }
    }

    #[test]
    fn hide_clause_without_show_round_trips() {
        let flow = assert_round_trips("REPORT REQUEST process HIDE(Error)\n");
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::Request { show, hide, .. }) => {
                assert!(show.is_empty());
                assert_eq!(hide, &vec!["Error".to_string()]);
            }
            other => panic!("expected report request, got {other:?}"),
        }
    }

    #[test]
    fn hide_with_block_round_trips() {
        // HIDE + WITH block: the WITH block opener must still be recognised.
        assert_round_trips(
            "REPORT REQUEST process HIDE(Response) WITH\n    result: jsonpath \"$.r\"\nEND\n",
        );
    }

    #[test]
    fn baseline_show_round_trips() {
        // SHOW(…) on BASELINE is parsed, preserved in the AST, and emitted back.
        let flow = assert_round_trips(
            "FOR TARGET IN ENVS BASELINE(\"p\") SHOW(Time), COMPARISON(\"s\")\n    REQUEST r\nEND\n",
        );
        if let FlowNode::ForEnvs { clause, .. } = &flow.nodes[0] {
            assert_eq!(
                clause,
                &EnvClause::Roles {
                    baseline: vec![RoleRef::Env("p".into())],
                    comparisons: vec![RoleRef::Env("s".into())],
                    baseline_show: vec!["Time".into()],
                }
            );
        } else {
            panic!("expected ForEnvs roles with SHOW");
        }
        // A SHOW field may carry its own STATISTICS(…), which is how a column
        // gets a summary without restating every other column in `# columns:`.
        assert_round_trips(
            "FOR TARGET IN ENVS BASELINE(\"p\") SHOW(Time STATISTICS(MEAN, MEDIAN)), COMPARISON(\"s\")\n    REQUEST r\nEND\n",
        );
        let f = parse_flow(
            "FOR TARGET IN ENVS BASELINE(\"p\") SHOW(Time STATISTICS(MEAN)), COMPARISON(\"s\")\n    REQUEST r\nEND\n",
        )
        .unwrap();
        let FlowNode::ForEnvs {
            clause: EnvClause::Roles { baseline_show, .. },
            ..
        } = &f.nodes[0]
        else {
            panic!("expected ForEnvs roles with SHOW");
        };
        assert_eq!(baseline_show[0].field, "Time");
        assert_eq!(baseline_show[0].stats, vec![StatKind::Mean]);
        // The same clause works on a REPORT statement's own SHOW.
        assert_round_trips("REPORT REQUEST r SHOW(status, Time STATISTICS(MEAN))\n");

        // Multiple SHOW fields also round-trip.
        assert_round_trips(
            "FOR TARGET IN ENVS BASELINE(\"p\") SHOW(Time, HttpStatus), COMPARISON(\"s\")\n    REQUEST r\nEND\n",
        );
    }

    #[test]
    fn report_statistics_clause_parses_and_round_trips() {
        use crate::report::model::StatKind;
        // `REPORT <var> AS <name> STATISTICS(a, b)` — the clause is parsed onto
        // the statement, collected into the flow's column stats by header, and
        // emitted back verbatim on serialization.
        let flow = assert_round_trips(
            "REPORT Time AS \"Response time\" STATISTICS(MEAN, MEDIAN)\nREPORT Overall AS Verdict STATISTICS(DISTRIBUTION)\n",
        );
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::VarAs {
                var, name, stats, ..
            }) => {
                assert_eq!(var, "Time");
                assert_eq!(name, "Response time");
                assert_eq!(stats, &vec![StatKind::Mean, StatKind::Median]);
            }
            other => panic!("expected VarAs with stats, got {other:?}"),
        }
        let cs = flow.column_stats();
        assert_eq!(
            cs.get("Response time"),
            Some(&vec![StatKind::Mean, StatKind::Median])
        );
        assert_eq!(cs.get("Verdict"), Some(&vec![StatKind::Distribution]));
    }

    #[test]
    fn report_image_clause_parses_and_round_trips() {
        use crate::report::flow::ImageSpec;
        // Bare `IMAGE`, each sizing option, and `FIT` all survive a parse →
        // serialize round-trip, and are collected by column header.
        let flow = assert_round_trips(
            "REPORT Frame AS Face IMAGE
REPORT Doc AS Page IMAGE(HEIGHT 110)
REPORT Sig AS Mark IMAGE(WIDTH 200, HEIGHT 100)
REPORT Thumb AS Small IMAGE(FIT)
",
        );
        let ci = flow.column_images();
        assert_eq!(ci.get("Face"), Some(&ImageSpec::default()));
        assert_eq!(
            ci.get("Page"),
            Some(&ImageSpec {
                height: Some(110),
                ..Default::default()
            })
        );
        assert_eq!(
            ci.get("Mark"),
            Some(&ImageSpec {
                width: Some(200),
                height: Some(100),
                fit: false
            })
        );
        assert_eq!(
            ci.get("Small"),
            Some(&ImageSpec {
                fit: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn report_image_and_statistics_parse_in_either_order() {
        use crate::report::flow::ImageSpec;
        use crate::report::model::StatKind;
        // Neither clause is more natural than the other, so both orders parse;
        // serialization normalises to STATISTICS-then-IMAGE.
        for src in [
            "REPORT Frame AS Face STATISTICS(COUNT) IMAGE(HEIGHT 60)
",
            "REPORT Frame AS Face IMAGE(HEIGHT 60) STATISTICS(COUNT)
",
        ] {
            let flow = parse_flow(src).expect("parse");
            assert_eq!(
                flow.column_stats().get("Face"),
                Some(&vec![StatKind::Count]),
                "{src}"
            );
            assert_eq!(
                flow.column_images().get("Face"),
                Some(&ImageSpec {
                    height: Some(60),
                    ..Default::default()
                }),
                "{src}"
            );
            assert_eq!(
                flow.to_text(),
                "REPORT Frame AS Face STATISTICS(COUNT) IMAGE(HEIGHT 60)\n",
                "both orders serialize the same way"
            );
        }
    }

    #[test]
    fn with_field_image_clause_parses_and_round_trips() {
        use crate::report::flow::ImageSpec;
        // The clause is available on a `WITH` field too, where the value is a
        // Hurl query rather than a variable.
        let flow = assert_round_trips(
            "REPORT REQUEST face WITH\n    Frame: jsonpath \"$.frame_url\" IMAGE(HEIGHT 110)\n    Status: HttpStatus\nEND\n",
        );
        assert_eq!(
            flow.column_images().get("face.Frame"),
            Some(&ImageSpec {
                height: Some(110),
                ..Default::default()
            })
        );
    }

    #[test]
    fn image_rejects_a_zero_or_missing_size() {
        // A zero-pixel picture is never what was meant, and an option list that
        // parses nothing should fail loudly rather than silently yielding a
        // bare IMAGE.
        for src in [
            "REPORT Frame AS Face IMAGE(HEIGHT 0)\n",
            "REPORT Frame AS Face IMAGE(HEIGHT)\n",
            "REPORT Frame AS Face IMAGE(TALL 10)\n",
        ] {
            assert!(parse_flow(src).is_err(), "{src} should not parse");
        }
    }

    /// A `TRUTH` clause is data, not an assertion, so it has to survive a
    /// round-trip through the editors untouched and reach `column_truths`.
    #[test]
    fn truth_clause_parses_and_round_trips_on_every_column_form() {
        for (src, key) in [
            (
                "REPORT Verdict AS Result TRUTH \"{{ expected }}\"\n",
                "Result",
            ),
            (
                "REPORT \"{{ a }}/{{ b }}\" AS Ratio TRUTH \"1/2\"\n",
                "Ratio",
            ),
            (
                "REPORT REQUEST face WITH\n    Verdict: jsonpath \"$.verdict\" TRUTH \"{{ expected }}\"\nEND\n",
                "face.Verdict",
            ),
        ] {
            let flow = assert_round_trips(src);
            assert!(
                flow.column_truths().contains_key(key),
                "{src} should record a truth for {key}"
            );
        }
    }

    /// `DETAIL` is placement, not content, so like `TRUTH` it has to survive a
    /// round-trip untouched and reach `column_details` from every column form.
    #[test]
    fn detail_flag_parses_and_round_trips_on_every_column_form() {
        for (src, key) in [
            ("REPORT Raw AS Payload DETAIL\n", "Payload"),
            ("REPORT \"{{ a }}/{{ b }}\" AS Ratio DETAIL\n", "Ratio"),
            (
                "REPORT REQUEST face WITH\n    Body: jsonpath \"$.body\" DETAIL\nEND\n",
                "face.Body",
            ),
        ] {
            let flow = assert_round_trips(src);
            assert!(
                flow.column_details().contains(key),
                "{src} should mark {key} as a detail column"
            );
        }
    }

    /// `DETAIL` serializes last, so it must still parse when written before the
    /// other clauses, and a column source ending in the word must be left alone.
    #[test]
    fn detail_parses_in_any_order_and_only_as_a_trailing_keyword() {
        let flow = parse_flow("REPORT V AS Verdict DETAIL TRUTH \"{{ e }}\"\n").expect("parse");
        assert!(flow.column_details().contains("Verdict"));
        assert_eq!(
            flow.to_text(),
            "REPORT V AS Verdict TRUTH \"{{ e }}\" DETAIL\n",
            "every order serializes the same way"
        );

        let flow = parse_flow("REPORT \"level of DETAIL\" AS Note\n").expect("parse");
        assert!(
            flow.column_details().is_empty(),
            "the word inside the quoted template is just text"
        );
    }

    /// The three trailing clauses are independent, so any order has to parse;
    /// serialization then normalises them to one order.
    #[test]
    fn truth_parses_in_any_clause_order_and_normalises() {
        for src in [
            "REPORT V AS Verdict STATISTICS(COUNT) IMAGE(HEIGHT 60) TRUTH \"{{ e }}\"\n",
            "REPORT V AS Verdict TRUTH \"{{ e }}\" IMAGE(HEIGHT 60) STATISTICS(COUNT)\n",
            "REPORT V AS Verdict IMAGE(HEIGHT 60) TRUTH \"{{ e }}\" STATISTICS(COUNT)\n",
        ] {
            let flow = parse_flow(src).expect("parse");
            assert_eq!(
                flow.column_truths().get("Verdict").map(String::as_str),
                Some("{{ e }}"),
                "{src}"
            );
            assert_eq!(
                flow.to_text(),
                "REPORT V AS Verdict STATISTICS(COUNT) IMAGE(HEIGHT 60) TRUTH \"{{ e }}\"\n",
                "every order serializes the same way"
            );
        }
    }

    /// The template is arbitrary text, so words that happen to be clause
    /// keywords inside it must stay part of the value, and an escaped quote
    /// must not end it early.
    #[test]
    fn truth_template_may_contain_clause_keywords_and_escaped_quotes() {
        let flow = parse_flow("REPORT V AS Verdict TRUTH \"IMAGE STATISTICS\"\n").expect("parse");
        assert_eq!(
            flow.column_truths().get("Verdict").map(String::as_str),
            Some("IMAGE STATISTICS"),
            "keywords inside the quotes are just text"
        );
        assert!(
            flow.column_images().is_empty(),
            "no IMAGE clause was written"
        );

        let flow =
            assert_round_trips("REPORT V AS Verdict TRUTH \"say \\\"hi\\\"\" STATISTICS(COUNT)\n");
        assert_eq!(
            flow.column_truths().get("Verdict").map(String::as_str),
            Some("say \"hi\""),
            "an escaped quote is part of the template"
        );
        assert_eq!(
            flow.column_stats().get("Verdict"),
            Some(&vec![StatKind::Count]),
            "the clause after it is still found"
        );
    }

    /// `TRUTH` needs a quoted argument; without one it is ordinary text, so the
    /// user sees what they typed rather than losing it to a silent no-op.
    #[test]
    fn truth_without_a_quoted_argument_is_not_a_clause() {
        let flow = parse_flow("REPORT V AS Verdict TRUTH pass\n");
        assert!(
            flow.is_err() || flow.unwrap().column_truths().is_empty(),
            "an unquoted TRUTH never becomes a clause"
        );
        // A column whose name merely starts with the word is untouched.
        let flow = parse_flow("REPORT V AS Truthy\n").expect("parse");
        assert!(flow.column_truths().is_empty());
    }

    #[test]
    fn report_statistics_without_as_uses_var_as_header() {
        use crate::report::model::StatKind;
        // Without an AS rename the variable name is the column header the stats
        // attach to.
        let flow = parse_flow("REPORT Time STATISTICS(SUM)\n").expect("parse");
        assert_eq!(flow.column_stats().get("Time"), Some(&vec![StatKind::Sum]));
    }
}
