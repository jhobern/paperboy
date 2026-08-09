//! The PaperTrail AST ([`ReportFlow`]) and its canonical text serializer.
//!
//! The serializer round-trips with [`super::parser::parse_flow`]: parsing then
//! serializing (or vice-versa) is stable. Keywords are emitted uppercase and
//! block bodies are indented four spaces per level — indentation is purely
//! cosmetic (the parser ignores it); `FOR … END` delimits blocks.
//!
//! See `docs/reports/02-grammar.md` for the grammar these types model.

use std::fmt::Write as _;

use super::model::StatKind;

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

/// One declared collection: a reference plus, for a helper, the alias its
/// requests are addressed through (`alias/request`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionRef<'a> {
    pub reference: &'a str,
    pub alias: Option<&'a str>,
}

/// Split `<ref> [AS <alias>]`.
///
/// The keyword is matched case-insensitively and only when it stands alone as
/// the second-to-last whitespace-separated word, so a path that merely contains
/// "as" (`./as-built/api.hurl`, `git:origin/as.hurl`) is not mangled. The alias
/// is returned unvalidated — checking it is an identifier, is present on every
/// helper and absent on the primary is validation's job, which can report a
/// useful message rather than silently treating the line as a plain path.
pub fn split_collection_ref(value: &str) -> (&str, Option<&str>) {
    let value = value.trim();
    let mut it = value.rsplitn(2, char::is_whitespace);
    let (Some(last), Some(head)) = (it.next(), it.next()) else {
        return (value, None);
    };
    let head = head.trim_end();
    if head
        .rsplit(char::is_whitespace)
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("AS"))
        && !last.is_empty()
    {
        let reference = head[..head.len() - 2].trim_end();
        if !reference.is_empty() {
            return (reference, Some(last));
        }
    }
    (value, None)
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

    /// Every value of the directives named `key`, in the order they were
    /// written. Repeatable directives (`collection:`) need all of them; `get`
    /// only ever sees the first.
    pub fn get_all(&self, key: &str) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HeaderLine::Directive { key: k, value } if k.eq_ignore_ascii_case(key) => {
                    Some(value.as_str())
                }
                _ => None,
            })
            .collect()
    }

    /// The bound collection reference (`collection:` directive), required for a
    /// runnable flow. This is the *primary* collection: the first one declared,
    /// with any `AS alias` suffix stripped. Helper collections are `collections()`.
    pub fn collection(&self) -> Option<&str> {
        self.get("collection").map(|v| split_collection_ref(v).0)
    }

    /// Every declared collection, in directive order, primary first.
    ///
    /// The primary carries no alias; each helper must (that is enforced by
    /// validation, not here, so the editors can still show a half-typed line).
    pub fn collections(&self) -> Vec<CollectionRef<'_>> {
        self.get_all("collection")
            .into_iter()
            .map(|v| {
                let (reference, alias) = split_collection_ref(v);
                CollectionRef { reference, alias }
            })
            .collect()
    }
    pub fn output(&self) -> Option<&str> {
        self.get("output")
    }
    /// The declared label classes (`labels:` directives), one per line, in the
    /// order written — which is also the order a confusion matrix's axes take.
    ///
    /// Repeatable, like `collection:`: each line declares one canonical label
    /// and its synonyms (`Pass = pass, ok, low risk`). Parsing them into a
    /// lookup is [`crate::report::labels::LabelMap`]'s job; the header only
    /// hands back the raw text, so a half-typed line in an editor is still
    /// round-tripped rather than dropped.
    pub fn labels(&self) -> Vec<&str> {
        self.get_all("labels")
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
}

/// One statement in a flow body (or loop body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowNode {
    /// `KEY=value` — set `{{KEY}}` in the current scope (includes `PRELUDE_*`).
    Assign { key: String, value: String },
    /// `LIST NAME = <producer>` — declare a named, iteration-only list.
    ListDecl { name: String, producer: Producer },
    /// A whole-line `# …` comment in the body.
    ///
    /// Comments are kept in the AST, not skipped as trivia, because every
    /// structural edit re-serializes the flow — so anything the AST can't hold
    /// is deleted the moment you touch the report in the node editor. Commenting
    /// a block out and losing it is the case that made this non-negotiable.
    ///
    /// Holds the text *after* the `#`, verbatim (leading space included), so a
    /// comment round-trips byte for byte.
    Comment(String),
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
    /// `REPORT REQUEST <name> [AS <alias>] [RESPONSE …] [SHOW(…)] [HIDE(…)] [WITH … END]`.
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
        /// `HIDE(a, b, …)`: remove these field suffixes from the final output
        /// after all other selection rules have been applied. Takes effect in
        /// every branch (SHOW, WITH-restricted, and default). Cannot overlap
        /// with `SHOW` (validation rejects the conflict).
        hide: Vec<String>,
        with: Vec<WithItem>,
    },
    /// `REPORT <var>` / `REPORT (<v1>, <v2>, …)` — one column per variable.
    Vars(Vec<String>),
    /// `REPORT <var> AS <name> [STATISTICS(…)]` — a single variable's value
    /// under a renamed column, with optional summary statistics. The bareword
    /// source is what distinguishes this from the quoted-template `Computed`
    /// form.
    VarAs {
        var: String,
        name: String,
        stats: Vec<StatKind>,
        image: Option<ImageSpec>,
        truth: Option<String>,
    },
    /// `REPORT "<template>" AS <name> [STATISTICS(…)] [IMAGE(…)]` — a computed
    /// column.
    Computed {
        template: String,
        name: String,
        stats: Vec<StatKind>,
        image: Option<ImageSpec>,
        truth: Option<String>,
    },
}

/// An `IMAGE[(HEIGHT n | WIDTH n | FIT, …)]` clause on a column.
///
/// This is a **render hint, never a value**: the cell's text stays exactly what
/// it was (a path, a URL, a base64 blob), and `IMAGE` only tells a writer that
/// can show pictures to draw that value as one. That is what keeps CSV and JSON
/// exports lossless, keeps baseline comparison textual, and lets a format with
/// no picture support degrade to the text automatically rather than needing a
/// fallback rule of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageSpec {
    /// Target height in pixels. With no `width`, the picture scales
    /// proportionally to this height.
    pub height: Option<u32>,
    /// Target width in pixels. With no `height`, the picture scales
    /// proportionally to this width.
    pub width: Option<u32>,
    /// `FIT`: size the picture to the cell rather than to a fixed box.
    pub fit: bool,
}

/// The height, in pixels, an `IMAGE` column's pictures are drawn at when the
/// clause names no size. Chosen to match the row height the reports this
/// feature was built for use, so a bare `IMAGE` produces a usable report.
pub const DEFAULT_IMAGE_HEIGHT: u32 = 110;

impl ImageSpec {
    /// The `(width, height)` box to scale a picture of `(w, h)` natural pixels
    /// into, preserving aspect ratio unless both dimensions were given.
    /// `None` for a `FIT` spec, whose sizing is the writer's business.
    pub fn scaled_size(&self, natural: (u32, u32)) -> Option<(f64, f64)> {
        if self.fit {
            return None;
        }
        let (nw, nh) = (natural.0.max(1) as f64, natural.1.max(1) as f64);
        Some(match (self.width, self.height) {
            (Some(w), Some(h)) => (w as f64, h as f64),
            (Some(w), None) => (w as f64, w as f64 * nh / nw),
            (None, Some(h)) => (h as f64 * nw / nh, h as f64),
            (None, None) => {
                let h = DEFAULT_IMAGE_HEIGHT as f64;
                (h * nw / nh, h)
            }
        })
    }
}

/// An item inside a `REPORT REQUEST … WITH … END` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithItem {
    ResponseFmt(ResponseFmt),
    /// `name: <hurl query> [STATISTICS(…)]` — an ad-hoc report field. The query
    /// is the same syntax as `[Reports]`, and may also be an intrinsic name
    /// (`HttpStatus`/`Time`/`Asserts`/`Error`/`Response`) to alias an intrinsic
    /// under a friendlier column name. An optional trailing `STATISTICS(…)`
    /// clause attaches summary statistics to the field's column.
    Field {
        name: String,
        query: String,
        stats: Vec<StatKind>,
        image: Option<ImageSpec>,
        truth: Option<String>,
    },
    /// A whole-line `#` comment written inside the block, kept so that
    /// commenting a field out doesn't destroy it the next time an editor
    /// re-serializes the flow. The text is stored exactly as written after the
    /// `#`, like [`FlowNode::Comment`].
    Comment(String),
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
    /// `FOLDERS "dir" [MATCH "glob"] [WITH role="glob"[?], …]`.
    ///
    /// `glob` filters subfolder *names* the way `FILES … MATCH` filters file
    /// names, and likewise recurses when it contains `**`.
    Folders {
        dir: String,
        glob: Option<String>,
        roles: Vec<RoleBinding>,
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

/// One `FOLDERS … WITH role="glob"` binding: the role name, the glob that picks
/// its file inside each folder, and whether the role is **optional**.
///
/// A required role must match exactly one file. An optional role (written with a
/// trailing `?`) may match none — it then binds the empty string, so a group
/// missing a genuinely optional input (a document with no back side, a folder
/// with no expected-result file) still produces a row instead of failing the
/// run. Matching *more* than one file is an error either way: ambiguity is never
/// silently resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleBinding {
    pub name: String,
    pub glob: String,
    pub optional: bool,
}

#[cfg(test)]
impl RoleBinding {
    /// A required role (the default form) -- a test convenience, since the
    /// parser and the editors always build the struct literally.
    pub fn required(name: impl Into<String>, glob: impl Into<String>) -> Self {
        RoleBinding {
            name: name.into(),
            glob: glob.into(),
            optional: false,
        }
    }
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

/// One environment role argument: either a named environment run live each
/// time, or a previously-exported snapshot loaded once and reused in place of a
/// live run. `FILE(…)` only appears in argument position inside a role clause
/// (`BASELINE(…)`/`COMPARISON(…)`), where a bare string would otherwise mean an
/// environment *name* — so it disambiguates "load this saved snapshot" from
/// "run this named environment". Every other path in the grammar (`FILES`,
/// `FOLDERS`, `TUPLES FROM`, header directives) is already unambiguously a path
/// by keyword/position and stays a bare string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleRef {
    /// A named environment, run live for this role.
    Env(String),
    /// A saved baseline snapshot file (resolved like producer paths, relative to
    /// `# root:`/the report dir). Its rows stand in for a live run of this role,
    /// so no environment is executed for it.
    File(String),
}

impl RoleRef {
    /// The comparison *target* identity this ref contributes: a named env is
    /// keyed by its name, a snapshot by its (relative) path. Used to align the
    /// injected/produced rows against the role sets in [`super::compare`].
    pub fn target(&self) -> &str {
        match self {
            RoleRef::Env(n) => n,
            RoleRef::File(p) => p,
        }
    }
}

/// The environment clause of `FOR … IN ENVS …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvClause {
    /// `"a", "b"` — iterate the named environments, no comparison.
    Plain(Vec<String>),
    /// `BASELINE("prod") SHOW(Time), COMPARISON("staging", …)`.
    ///
    /// Each role argument is a [`RoleRef`]: a live environment name or a
    /// `FILE("…")` snapshot to reuse in place of running it.
    ///
    /// `baseline_show` names the baseline fields to copy into each candidate row
    /// under `baseline.<alias>.<field>` (only for aliases the candidate already
    /// emits that field).  Empty when no `SHOW(…)` clause is present.
    Roles {
        baseline: Vec<RoleRef>,
        comparisons: Vec<RoleRef>,
        baseline_show: Vec<String>,
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
                // Verbatim: the text already holds whatever spacing followed
                // the `#` (see the parser), so it must not be re-padded here.
                HeaderLine::Comment(c) => {
                    let _ = writeln!(out, "#{c}");
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

    /// Collect the per-column summary statistics requested by
    /// `REPORT … AS <header> STATISTICS(…)` statements anywhere in the flow
    /// (including inside loops), keyed by output-column header. Later statements
    /// for the same header win. Used to attach statistics to the resolved
    /// columns at render time.
    pub fn column_stats(&self) -> std::collections::HashMap<String, Vec<StatKind>> {
        let mut out = std::collections::HashMap::new();
        collect_column_stats(&self.nodes, &mut out);
        out
    }

    /// Collect the per-column `IMAGE[(…)]` render hints requested anywhere in
    /// the flow, keyed by output-column header, exactly as
    /// [`column_stats`](Self::column_stats) does for statistics — the two
    /// clauses attach at the same three places and are resolved the same way.
    pub fn column_images(&self) -> std::collections::HashMap<String, ImageSpec> {
        let mut out = std::collections::HashMap::new();
        collect_column_images(&self.nodes, &mut out);
        out
    }

    /// Collect the per-column `TRUTH "<template>"` clauses declared anywhere in
    /// the flow, keyed by output-column header, exactly as
    /// [`column_images`](Self::column_images) does — the three clauses attach at
    /// the same three places and are resolved the same way.
    ///
    /// The value is the **unevaluated template**. It is interpolated per row,
    /// after the run, against that row's variable snapshot: a ground truth is by
    /// definition something known before the request was sent, so it is read
    /// from the loop that chose the input (a labels manifest, a folder name),
    /// never from the response it is judging.
    pub fn column_truths(&self) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        collect_column_truths(&self.nodes, &mut out);
        out
    }
}

fn collect_column_images(
    nodes: &[FlowNode],
    out: &mut std::collections::HashMap<String, ImageSpec>,
) {
    for node in nodes {
        match node {
            FlowNode::Report(ReportStmt::VarAs { name, image, .. })
            | FlowNode::Report(ReportStmt::Computed { name, image, .. }) => {
                if let Some(img) = image {
                    out.insert(name.clone(), *img);
                }
            }
            FlowNode::Report(ReportStmt::Request {
                name, alias, with, ..
            }) => {
                let a = alias
                    .clone()
                    .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name).to_string());
                for item in with {
                    if let WithItem::Field {
                        name: fname,
                        image: Some(img),
                        ..
                    } = item
                    {
                        out.insert(format!("{a}.{fname}"), *img);
                    }
                }
            }
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => {
                collect_column_images(body, out);
            }
            _ => {}
        }
    }
}

fn collect_column_truths(nodes: &[FlowNode], out: &mut std::collections::HashMap<String, String>) {
    for node in nodes {
        match node {
            FlowNode::Report(ReportStmt::VarAs { name, truth, .. })
            | FlowNode::Report(ReportStmt::Computed { name, truth, .. }) => {
                if let Some(t) = truth {
                    out.insert(name.clone(), t.clone());
                }
            }
            FlowNode::Report(ReportStmt::Request {
                name, alias, with, ..
            }) => {
                let a = alias
                    .clone()
                    .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name).to_string());
                for item in with {
                    if let WithItem::Field {
                        name: fname,
                        truth: Some(t),
                        ..
                    } = item
                    {
                        out.insert(format!("{a}.{fname}"), t.clone());
                    }
                }
            }
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => {
                collect_column_truths(body, out);
            }
            _ => {}
        }
    }
}

fn collect_column_stats(
    nodes: &[FlowNode],
    out: &mut std::collections::HashMap<String, Vec<StatKind>>,
) {
    for node in nodes {
        match node {
            FlowNode::Report(ReportStmt::VarAs { name, stats, .. })
            | FlowNode::Report(ReportStmt::Computed { name, stats, .. })
                if !stats.is_empty() =>
            {
                out.insert(name.clone(), stats.clone());
            }
            // `WITH` fields carry their own optional `STATISTICS(…)`; their
            // output column is `alias.field`, where `alias` defaults to the
            // request's leaf name. Compute that key statically so the stats
            // attach at render time just like a `REPORT … STATISTICS(…)`.
            FlowNode::Report(ReportStmt::Request {
                name, alias, with, ..
            }) => {
                let a = alias
                    .clone()
                    .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name).to_string());
                for item in with {
                    if let WithItem::Field {
                        name: fname, stats, ..
                    } = item
                        && !stats.is_empty()
                    {
                        out.insert(format!("{a}.{fname}"), stats.clone());
                    }
                }
            }
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => {
                collect_column_stats(body, out);
            }
            _ => {}
        }
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
        FlowNode::Comment(text) => {
            let _ = writeln!(out, "#{text}");
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
            hide,
            with,
        } => {
            let _ = write!(out, "REPORT REQUEST {}", name_text(name));
            if let Some(a) = alias {
                let _ = write!(out, " AS {}", name_text(a));
            }
            if let Some(fmt) = response_fmt {
                let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
            }
            if !show.is_empty() {
                let _ = write!(out, " SHOW({})", show.join(", "));
            }
            if !hide.is_empty() {
                let _ = write!(out, " HIDE({})", hide.join(", "));
            }
            if with.is_empty() {
                out.push('\n');
            } else {
                out.push_str(" WITH\n");
                for item in with {
                    indent(out, depth + 1);
                    out.push_str(&with_item_text(item));
                    out.push('\n');
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
        ReportStmt::VarAs {
            var,
            name,
            stats,
            image,
            truth,
        } => {
            let _ = writeln!(
                out,
                "REPORT {var} AS {}{}{}{}",
                name_text(name),
                stats_text(stats),
                image_text(image.as_ref()),
                truth_text(truth.as_deref())
            );
        }
        ReportStmt::Computed {
            template,
            name,
            stats,
            image,
            truth,
        } => {
            let _ = writeln!(
                out,
                "REPORT {} AS {}{}{}{}",
                quote(template),
                name_text(name),
                stats_text(stats),
                image_text(image.as_ref()),
                truth_text(truth.as_deref())
            );
        }
    }
}

/// Render a `STATISTICS(…)` clause (with a leading space) for a report
/// statement, or the empty string when no statistics are requested.
/// One `WITH` item as it is written inside the block (no indentation, no
/// newline). Shared with the node editor so an outline row and the source line
/// it stands for can't drift apart.
pub(crate) fn with_item_text(item: &WithItem) -> String {
    match item {
        WithItem::ResponseFmt(fmt) => format!("RESPONSE {}", fmt_text(*fmt)),
        WithItem::Comment(text) => format!("#{text}"),
        WithItem::Field {
            name,
            query,
            stats,
            image,
            truth,
        } => format!(
            "{}: {query}{}{}{}",
            name_text(name),
            stats_text(stats),
            image_text(image.as_ref()),
            truth_text(truth.as_deref())
        ),
    }
}

fn stats_text(stats: &[StatKind]) -> String {
    if stats.is_empty() {
        return String::new();
    }
    let list: Vec<&str> = stats.iter().map(|s| s.keyword()).collect();
    format!(" STATISTICS({})", list.join(", "))
}

/// Render an `IMAGE[(…)]` clause (with a leading space), or the empty string
/// when the column carries none. A spec with no options round-trips as the bare
/// keyword rather than `IMAGE()`, which is how it is written.
pub(crate) fn image_text(image: Option<&ImageSpec>) -> String {
    let Some(img) = image else {
        return String::new();
    };
    let mut opts: Vec<String> = Vec::new();
    if img.fit {
        opts.push("FIT".to_string());
    }
    if let Some(w) = img.width {
        opts.push(format!("WIDTH {w}"));
    }
    if let Some(h) = img.height {
        opts.push(format!("HEIGHT {h}"));
    }
    if opts.is_empty() {
        " IMAGE".to_string()
    } else {
        format!(" IMAGE({})", opts.join(", "))
    }
}

/// Render a `TRUTH "<template>"` clause (with a leading space), or the empty
/// string when the column declares no ground truth.
///
/// The template is re-quoted rather than written verbatim because it is a
/// string literal in the grammar: a truth of `{{ expected }}` and one of
/// `pass` are both perfectly ordinary values, and only the quotes tell them
/// apart from the keywords around them.
pub(crate) fn truth_text(truth: Option<&str>) -> String {
    match truth {
        Some(t) => format!(" TRUTH {}", quote(t)),
        None => String::new(),
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
        Producer::Folders { dir, glob, roles } => {
            let mut out = format!("FOLDERS {}", quote(dir));
            if let Some(g) = glob {
                out.push_str(&format!(" MATCH {}", quote(g)));
            }
            if !roles.is_empty() {
                let rs: Vec<String> = roles
                    .iter()
                    .map(|r| {
                        let opt = if r.optional { "?" } else { "" };
                        format!("{}={}{opt}", r.name, quote(&r.glob))
                    })
                    .collect();
                out.push_str(&format!(" WITH {}", rs.join(", ")));
            }
            out
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
            baseline_show,
        } => {
            let mut parts = Vec::new();
            if !baseline.is_empty() {
                let names: Vec<String> = baseline.iter().map(role_ref_text).collect();
                let mut token = format!("BASELINE({})", names.join(", "));
                if !baseline_show.is_empty() {
                    token.push_str(&format!(" SHOW({})", baseline_show.join(", ")));
                }
                parts.push(token);
            }
            if !comparisons.is_empty() {
                let names: Vec<String> = comparisons.iter().map(role_ref_text).collect();
                parts.push(format!("COMPARISON({})", names.join(", ")));
            }
            parts.join(", ")
        }
    }
}

/// Render a single role argument: a bare quoted env name, or `FILE("…")` for a
/// snapshot reference.
fn role_ref_text(r: &RoleRef) -> String {
    match r {
        RoleRef::Env(n) => quote(n),
        RoleRef::File(p) => format!("FILE({})", quote(p)),
    }
}

/// Render a request/column name: bare when it is a valid bareword, quoted when
/// it is empty or contains any character the parser's `word` production would
/// stop at (whitespace or one of `()[],="`), so it always re-parses as one
/// name.
fn name_text(name: &str) -> String {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || "()[],=\"".contains(c))
    {
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
            FlowNode::Comment(text) => format!("#{text}"),
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
                hide,
                ..
            }) => {
                let mut out = format!("REPORT REQUEST {}", name_text(name));
                if let Some(a) = alias {
                    let _ = write!(out, " AS {}", name_text(a));
                }
                if let Some(fmt) = response_fmt {
                    let _ = write!(out, " RESPONSE {}", fmt_text(*fmt));
                }
                if !show.is_empty() {
                    let _ = write!(out, " SHOW({})", show.join(", "));
                }
                if !hide.is_empty() {
                    let _ = write!(out, " HIDE({})", hide.join(", "));
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
            hide,
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
            if !hide.is_empty() {
                let _ = write!(out, " HIDE({})", hide.join(", "));
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
        ReportStmt::VarAs {
            var,
            name,
            stats,
            image,
            truth,
        } => {
            format!(
                "REPORT {var} AS {name}{}{}{}",
                stats_text(stats),
                image_text(image.as_ref()),
                truth_text(truth.as_deref())
            )
        }
        ReportStmt::Computed {
            template,
            name,
            stats,
            image,
            truth,
        } => {
            format!(
                "REPORT {} AS {name}{}{}{}",
                quote(template),
                stats_text(stats),
                image_text(image.as_ref()),
                truth_text(truth.as_deref())
            )
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
