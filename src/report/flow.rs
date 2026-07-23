//! The PaperTrail AST ([`ReportFlow`]) and its canonical text serializer.
//!
//! The serializer round-trips with [`super::parser::parse_flow`]: parsing then
//! serializing (or vice-versa) is stable. Keywords are emitted uppercase and
//! block bodies are indented four spaces per level — indentation is purely
//! cosmetic (the parser ignores it); `FOR … END` delimits blocks.
//!
//! See `docs/reports/02-grammar.md` for the grammar these types model.

use std::fmt::Write as _;

/// A whole report flow: a comment/directive header plus the ordered statements
/// the interpreter executes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportFlow {
    pub header: Header,
    pub nodes: Vec<FlowNode>,
}

/// The header block: the `# key: value` directives (and any free `#` comments)
/// that precede the first statement. Stored as an ordered list so it
/// round-trips verbatim; typed access to known directives is via the helper
/// methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub lines: Vec<HeaderLine>,
}

/// One line of the header: either a recognised `# key: value` directive or a
/// free-form `#` comment (preserved as-is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderLine {
    Directive { key: String, value: String },
    Comment(String),
}

impl Header {
    /// The value of the first directive named `key` (case-insensitive), if any.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            HeaderLine::Directive { key: k, value } if k.eq_ignore_ascii_case(key) => {
                Some(value.as_str())
            }
            _ => None,
        })
    }

    /// The bound collection reference (`collection:` directive), required for a
    /// runnable flow.
    pub fn collection(&self) -> Option<&str> {
        self.get("collection")
    }
    pub fn name(&self) -> Option<&str> {
        self.get("name")
    }
    pub fn output(&self) -> Option<&str> {
        self.get("output")
    }
    pub fn columns(&self) -> Option<&str> {
        self.get("columns")
    }
    pub fn root(&self) -> Option<&str> {
        self.get("root")
    }
}

/// One statement in a flow body (or loop body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowNode {
    /// `KEY=value` — set `{{KEY}}` in the current scope (includes `PRELUDE_*`).
    Assign { key: String, value: String },
    /// `LIST NAME = <producer>` — declare a named, iteration-only list.
    ListDecl { name: String, producer: Producer },
    /// `REQUEST <name>` — send a request, emit no column.
    Request { name: String },
    /// `REPORT …` — send/compute and emit column(s) into the current row.
    Report(ReportStmt),
    /// `[PARALLEL[(n)]] FOR <pattern> IN <producer> … END`.
    ForEach {
        pattern: Pattern,
        producer: Producer,
        body: Vec<FlowNode>,
        /// `Some(..)` when the loop is marked `PARALLEL`: its iterations run
        /// concurrently, each on an independent snapshot of the enclosing
        /// scope, with rows still emitted in iteration order. `None` = the
        /// default sequential loop.
        parallel: Option<ParallelSpec>,
    },
    /// `[PARALLEL[(n)]] FOR <var> IN ENVS <clause> … END`.
    ForEnvs {
        var: String,
        clause: EnvClause,
        body: Vec<FlowNode>,
        parallel: Option<ParallelSpec>,
    },
}

/// The `PARALLEL` marker on a loop: run its iterations concurrently.
///
/// Iterations are independent — each gets its own snapshot of the enclosing
/// scope and its own forward capture chain, so a body like
/// `create → upload → process` still runs sequentially *within* one iteration.
/// Results are buffered by iteration index and emitted in order, so the report
/// is deterministic no matter which iteration finishes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParallelSpec {
    /// An explicit worker cap from `PARALLEL(n)`. `None` means "use the engine
    /// default" (`PRELUDE_MAX_PARALLEL`, itself defaulting to a built-in cap).
    pub degree: Option<u32>,
}

/// The column-emitting `REPORT` statement in its three forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportStmt {
    /// `REPORT REQUEST <name> [AS <alias>] [RESPONSE …] [WITH … END]`.
    Request {
        name: String,
        alias: Option<String>,
        response_fmt: Option<ResponseFmt>,
        with: Vec<WithItem>,
    },
    /// `REPORT <var>` / `REPORT (<v1>, <v2>, …)` — one column per variable.
    Vars(Vec<String>),
    /// `REPORT "<template>" AS <name>` — a computed column.
    Computed { template: String, name: String },
}

/// An item inside a `REPORT REQUEST … WITH … END` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithItem {
    ResponseFmt(ResponseFmt),
    /// `name: <hurl query>` — an ad-hoc report field (same syntax as `[Reports]`).
    Field {
        name: String,
        query: String,
    },
}

/// How a reported response body is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFmt {
    Raw,
    Pretty,
}

/// Anything a `FOR` can iterate (the `ENVS` special form aside).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Producer {
    /// `[ … ]` literal list of scalars/tuples.
    List(Vec<Element>),
    /// `FILES "dir" [MATCH "glob"]`.
    Files { dir: String, glob: Option<String> },
    /// `FOLDERS "dir" [WITH role="glob", …]`.
    Folders {
        dir: String,
        roles: Vec<(String, String)>,
    },
    /// `TUPLES FROM "file"`.
    Tuples { path: String },
    /// `ZIP(a, b, …)`.
    Zip(Vec<Producer>),
    /// A previously declared `LIST` referenced by name.
    Named(String),
}

/// One element of a list literal: a scalar or a tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    Scalar(String),
    Tuple(Vec<String>),
}

/// A destructuring pattern on the left of `FOR … IN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub binders: Vec<Binder>,
    /// `true` when the pattern ends with `...` (absorb trailing positions).
    pub rest: bool,
}

/// One position in a [`Pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binder {
    Named(String),
    /// `_` — discard this position.
    Discard,
}

impl Pattern {
    /// A single-binder pattern (`FOR X IN …`).
    pub fn single(name: impl Into<String>) -> Self {
        Pattern {
            binders: vec![Binder::Named(name.into())],
            rest: false,
        }
    }
    /// Whether this is exactly one binder (arity-1 producer form).
    pub fn is_single(&self) -> bool {
        self.binders.len() == 1 && !self.rest
    }
    /// The named binders (skipping `_`), in order — the variables this pattern
    /// introduces into scope.
    pub fn named(&self) -> impl Iterator<Item = &str> {
        self.binders.iter().filter_map(|b| match b {
            Binder::Named(n) => Some(n.as_str()),
            Binder::Discard => None,
        })
    }
}

/// The environment clause of `FOR … IN ENVS …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvClause {
    /// `"a", "b"` — iterate the named environments, no comparison.
    Plain(Vec<String>),
    /// `BASELINE("prod"), COMPARISON("staging", …)`.
    Roles {
        baseline: Vec<String>,
        comparisons: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

const INDENT: &str = "    ";

impl ReportFlow {
    /// Serialize to canonical PaperTrail text (round-trips with `parse_flow`).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for line in &self.header.lines {
            match line {
                HeaderLine::Directive { key, value } => {
                    let _ = writeln!(out, "# {key}: {value}");
                }
                HeaderLine::Comment(c) => {
                    let _ = writeln!(out, "# {c}");
                }
            }
        }
        // Blank line between a non-empty header and the body.
        if !self.header.lines.is_empty() && !self.nodes.is_empty() {
            out.push('\n');
        }
        for node in &self.nodes {
            write_node(&mut out, node, 0);
        }
        out
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

fn write_node(out: &mut String, node: &FlowNode, depth: usize) {
    indent(out, depth);
    match node {
        FlowNode::Assign { key, value } => {
            let _ = writeln!(out, "{key}={value}");
        }
        FlowNode::ListDecl { name, producer } => {
            let _ = writeln!(out, "LIST {name} = {}", producer_text(producer));
        }
        FlowNode::Request { name } => {
            let _ = writeln!(out, "REQUEST {}", name_text(name));
        }
        FlowNode::Report(stmt) => write_report(out, stmt, depth),
        FlowNode::ForEach {
            pattern,
            producer,
            body,
            parallel,
        } => {
            let _ = writeln!(
                out,
                "{}FOR {} IN {}",
                parallel_prefix(parallel),
                pattern_text(pattern),
                producer_text(producer)
            );
            for n in body {
                write_node(out, n, depth + 1);
            }
            indent(out, depth);
            out.push_str("END\n");
        }
        FlowNode::ForEnvs {
            var,
            clause,
            body,
            parallel,
        } => {
            let _ = writeln!(
                out,
                "{}FOR {var} IN ENVS {}",
                parallel_prefix(parallel),
                env_clause_text(clause)
            );
            for n in body {
                write_node(out, n, depth + 1);
            }
            indent(out, depth);
            out.push_str("END\n");
        }
    }
}

/// The `PARALLEL[(n)] ` prefix a loop serializes with (empty when sequential).
fn parallel_prefix(p: &Option<ParallelSpec>) -> String {
    match p {
        None => String::new(),
        Some(ParallelSpec { degree: None }) => "PARALLEL ".to_string(),
        Some(ParallelSpec { degree: Some(n) }) => format!("PARALLEL({n}) "),
    }
}

fn write_report(out: &mut String, stmt: &ReportStmt, depth: usize) {
    match stmt {
        ReportStmt::Request {
            name,
            alias,
            response_fmt,
            with,
        } => {
            let _ = write!(out, "REPORT REQUEST {}", name_text(name));
            if let Some(a) = alias {
                let _ = write!(out, " AS {a}");
            }
            if let Some(fmt) = response_fmt {
                let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
            }
            if with.is_empty() {
                out.push('\n');
            } else {
                out.push_str(" WITH\n");
                for item in with {
                    indent(out, depth + 1);
                    match item {
                        WithItem::ResponseFmt(fmt) => {
                            let _ = writeln!(out, "RESPONSE {}", fmt_text(*fmt));
                        }
                        WithItem::Field { name, query } => {
                            let _ = writeln!(out, "{name}: {query}");
                        }
                    }
                }
                indent(out, depth);
                out.push_str("END\n");
            }
        }
        ReportStmt::Vars(vars) => {
            if vars.len() == 1 {
                let _ = writeln!(out, "REPORT {}", vars[0]);
            } else {
                let _ = writeln!(out, "REPORT ({})", vars.join(", "));
            }
        }
        ReportStmt::Computed { template, name } => {
            let _ = writeln!(out, "REPORT {} AS {name}", quote(template));
        }
    }
}

fn fmt_text(fmt: ResponseFmt) -> &'static str {
    match fmt {
        ResponseFmt::Raw => "RAW",
        ResponseFmt::Pretty => "PRETTY",
    }
}

fn pattern_text(p: &Pattern) -> String {
    if p.is_single() {
        return binder_text(&p.binders[0]);
    }
    let mut parts: Vec<String> = p.binders.iter().map(binder_text).collect();
    if p.rest {
        parts.push("...".to_string());
    }
    format!("({})", parts.join(", "))
}

fn binder_text(b: &Binder) -> String {
    match b {
        Binder::Named(n) => n.clone(),
        Binder::Discard => "_".to_string(),
    }
}

fn producer_text(p: &Producer) -> String {
    match p {
        Producer::List(elems) => {
            let items: Vec<String> = elems.iter().map(element_text).collect();
            format!("[{}]", items.join(", "))
        }
        Producer::Files { dir, glob } => match glob {
            Some(g) => format!("FILES {} MATCH {}", quote(dir), quote(g)),
            None => format!("FILES {}", quote(dir)),
        },
        Producer::Folders { dir, roles } => {
            if roles.is_empty() {
                format!("FOLDERS {}", quote(dir))
            } else {
                let rs: Vec<String> = roles
                    .iter()
                    .map(|(k, v)| format!("{k}={}", quote(v)))
                    .collect();
                format!("FOLDERS {} WITH {}", quote(dir), rs.join(", "))
            }
        }
        Producer::Tuples { path } => format!("TUPLES FROM {}", quote(path)),
        Producer::Zip(ps) => {
            let items: Vec<String> = ps.iter().map(producer_text).collect();
            format!("ZIP({})", items.join(", "))
        }
        Producer::Named(n) => n.clone(),
    }
}

fn element_text(e: &Element) -> String {
    match e {
        Element::Scalar(s) => quote(s),
        Element::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(|s| quote(s)).collect();
            format!("({})", parts.join(", "))
        }
    }
}

fn env_clause_text(c: &EnvClause) -> String {
    match c {
        EnvClause::Plain(names) => names
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(", "),
        EnvClause::Roles {
            baseline,
            comparisons,
        } => {
            let mut parts = Vec::new();
            if !baseline.is_empty() {
                let names: Vec<String> = baseline.iter().map(|s| quote(s)).collect();
                parts.push(format!("BASELINE({})", names.join(", ")));
            }
            if !comparisons.is_empty() {
                let names: Vec<String> = comparisons.iter().map(|s| quote(s)).collect();
                parts.push(format!("COMPARISON({})", names.join(", ")));
            }
            parts.join(", ")
        }
    }
}

/// Render a request/column name: bare when it's a simple token, quoted when it
/// contains whitespace (so it re-parses as one name).
fn name_text(name: &str) -> String {
    if name.chars().any(char::is_whitespace) || name.is_empty() {
        quote(name)
    } else {
        name.to_string()
    }
}

/// Double-quote a string, escaping `\` and `"`.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
