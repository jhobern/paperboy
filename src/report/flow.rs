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

pub enum HeaderDirective {
    Collection(String),
    Name(String),
    Output(String),
    Columns(String),
    Root(String),
    Environment(String),
    Baseline(String),
    // Any directive that doesn't match one of the declared types is stored as (key, value)
    Unknown(String, String),
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
    /// The saved-run snapshot (`baseline:` directive) to diff this run against —
    /// PaperTrail's "Source B" comparison. Names a `.baseline` JSON file (a
    /// previous run saved via the results grid) whose reported fields are diffed
    /// against the current run to produce the `Result` column, exactly like an
    /// `ENVS BASELINE/COMPARISON` clause but against stored values rather than a
    /// live baseline environment. The path resolves like producer paths
    /// (relative to `# root:` / the report's directory). Ignored when the flow
    /// already configures an `ENVS` role comparison (that takes precedence).
    pub fn baseline(&self) -> Option<&str> {
        self.get("baseline")
    }
    /// The single environment (`environment:` directive) to use as the report's
    /// base variable layer for a plain, no-comparison run. Names an
    /// *already-loaded* global environment (validation errors if it isn't
    /// loaded); when absent the run falls back to the app's active + the bound
    /// collection's pinned environment. Multi-environment comparison still uses
    /// a `FOR … IN ENVS` loop, not this directive.
    pub fn environment(&self) -> Option<&str> {
        self.get("environment")
    }

    /// Set (or insert) the directive named `key` to `value`, preserving the
    /// position of an existing directive and appending a new one otherwise.
    /// Used when the user re-points a report (e.g. BIND changes `collection:`).
    /// Matching is case-insensitive; the stored key is left as-was on update and
    /// written lowercase for a fresh directive (matching the serializer style).
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        for line in &mut self.lines {
            if let HeaderLine::Directive { key: k, value: v } = line
                && k.eq_ignore_ascii_case(key)
            {
                *v = value;
                return;
            }
        }
        self.lines.push(HeaderLine::Directive {
            key: key.to_ascii_lowercase(),
            value,
        });
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
    /// `REPORT REQUEST <name> [AS <alias>] [RESPONSE …] [SHOW(…)] [WITH … END]`.
    Request {
        name: String,
        alias: Option<String>,
        response_fmt: Option<ResponseFmt>,
        /// The per-statement field selector `SHOW(a, b, …)`: when non-empty,
        /// only these field suffixes (intrinsics like `Time` and/or
        /// `[Reports]`/`WITH` field names) are emitted, in listed order — so a
        /// noisy `Response` (e.g. a base64 blob) can be dropped. Empty = no
        /// `SHOW` clause, i.e. emit every field (the default).
        show: Vec<String>,
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
    /// `CONCAT(a, b, …)` — the items of each input appended end-to-end into one
    /// longer stream (all inputs must share the same arity).
    Concat(Vec<Producer>),
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
            show,
            with,
        } => {
            let _ = write!(out, "REPORT REQUEST {}", name_text(name));
            if let Some(a) = alias {
                let _ = write!(out, " AS {a}");
            }
            if let Some(fmt) = response_fmt {
                let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
            }
            if !show.is_empty() {
                let _ = write!(out, " SHOW({})", show.join(", "));
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
        Producer::Concat(ps) => {
            let items: Vec<String> = ps.iter().map(producer_text).collect();
            format!("CONCAT({})", items.join(", "))
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

// ---------------------------------------------------------------------------
// Single-node views (for the structured node editor)
// ---------------------------------------------------------------------------

impl FlowNode {
    /// A concise, human-readable one-line label for this node — what the
    /// structured ("node") editor shows for it in the outline. For a loop this
    /// is only the `FOR … IN …` opener (its body and `END` are separate rows);
    /// a `REPORT REQUEST … WITH …` is summarised with a trailing `WITH …`.
    pub fn label(&self) -> String {
        match self {
            FlowNode::Assign { key, value } => format!("{key} = {value}"),
            FlowNode::ListDecl { name, producer } => {
                format!("LIST {name} = {}", producer_text(producer))
            }
            FlowNode::Request { name } => format!("REQUEST {name}"),
            FlowNode::Report(stmt) => report_label(stmt),
            FlowNode::ForEach {
                pattern,
                producer,
                parallel,
                ..
            } => format!(
                "{}FOR {} IN {}",
                parallel_prefix(parallel),
                pattern_text(pattern),
                producer_text(producer)
            ),
            FlowNode::ForEnvs {
                var,
                clause,
                parallel,
                ..
            } => format!(
                "{}FOR {var} IN ENVS {}",
                parallel_prefix(parallel),
                env_clause_text(clause)
            ),
        }
    }

    /// The re-parseable single-line source form of this node's *header* — the
    /// node itself for leaf statements, or the `FOR … IN …` opener for a loop
    /// (its body and `END` excluded). Used by the node editor's "edit as line"
    /// prompt: the returned text, followed by `END` for a loop, round-trips
    /// through [`super::parser::parse_flow`]. A `REPORT REQUEST … WITH …` block
    /// is *not* representable on one line, so its `WITH` items are dropped here
    /// (request nodes are edited via the request picker, not this line form).
    pub fn header_line(&self) -> String {
        match self {
            FlowNode::Report(ReportStmt::Request {
                name,
                alias,
                response_fmt,
                show,
                ..
            }) => {
                let mut out = format!("REPORT REQUEST {}", name_text(name));
                if let Some(a) = alias {
                    let _ = write!(out, " AS {a}");
                }
                if let Some(fmt) = response_fmt {
                    let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
                }
                if !show.is_empty() {
                    let _ = write!(out, " SHOW({})", show.join(", "));
                }
                out
            }
            _ => self.label(),
        }
    }

    /// The request title this node references, if it is a `REQUEST` or
    /// `REPORT REQUEST` node — used by the node editor to colour the row by
    /// whether the name resolves in the bound collection.
    pub fn request_name(&self) -> Option<&str> {
        match self {
            FlowNode::Request { name } => Some(name),
            FlowNode::Report(ReportStmt::Request { name, .. }) => Some(name),
            _ => None,
        }
    }

    /// Whether this node opens a loop block (`FOR …`), i.e. it carries a body
    /// and closes with an `END`. The node editor renders these with a matching
    /// `END` row and nests their body one level deeper.
    pub fn is_loop(&self) -> bool {
        matches!(self, FlowNode::ForEach { .. } | FlowNode::ForEnvs { .. })
    }

    /// The loop body of a `FOR …` node (mutable), or `None` for a leaf node.
    pub fn body_mut(&mut self) -> Option<&mut Vec<FlowNode>> {
        match self {
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// The label for a [`ReportStmt`] (see [`FlowNode::label`]).
fn report_label(stmt: &ReportStmt) -> String {
    match stmt {
        ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            with,
        } => {
            let mut out = format!("REPORT REQUEST {name}");
            if let Some(a) = alias {
                let _ = write!(out, " AS {a}");
            }
            if let Some(fmt) = response_fmt {
                let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
            }
            if !show.is_empty() {
                let _ = write!(out, " SHOW({})", show.join(", "));
            }
            if !with.is_empty() {
                out.push_str(" WITH …");
            }
            out
        }
        ReportStmt::Vars(vars) => {
            if vars.len() == 1 {
                format!("REPORT {}", vars[0])
            } else {
                format!("REPORT ({})", vars.join(", "))
            }
        }
        ReportStmt::Computed { template, name } => {
            format!("REPORT {} AS {name}", quote(template))
        }
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
