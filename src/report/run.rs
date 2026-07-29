//! The PaperTrail interpreter: walk a [`ReportFlow`] and produce a
//! [`ReportResult`] (the wide row model in [`super::model`]).
//!
//! # Design
//!
//! The interpreter is **front-end agnostic** and, crucially, **runner
//! agnostic**: every actual HTTP send goes through the [`EntryRunner`] trait so
//! the whole engine is unit-testable without a network (tests inject a fake
//! runner that returns canned responses). The production implementation
//! ([`LiveRunner`]) wraps [`crate::request::run_resolved_entry`], the exact
//! single-request pipeline the TUI uses, so a report request is sent
//! identically to a hand-run one.
//!
//! # Rows
//!
//! The output is *wide*: a **row** is one innermost-loop iteration (or the one
//! row of a loop-free flow). Each `REPORT` statement contributes namespaced
//! cells to the current row(s); a `REPORT` at an outer level (e.g. before a
//! loop) is evaluated once and **broadcast** into every row the block produces.
//! [`Exec::exec_block`] implements this by accumulating this-level cells and
//! merging them into the rows returned by any nested loops (or emitting a single
//! row when the block has no loop).
//!
//! # Report fields are evaluated natively, not as captures
//!
//! `[Reports]`/`WITH` fields look like `[Captures]` (`name: <hurl query>`) but
//! are **not** run as captures. Hurl captures are all-or-nothing: a single
//! query that matches nothing aborts the entry and discards *every* capture
//! (verified against `hurl` 8.0.1), which would both break capture chaining and
//! violate the "always emit a row, show a no-match marker" contract. Hurl also
//! does not expose its query evaluator publicly. So report fields are evaluated
//! *tolerantly* against the response we already have (see [`eval_field`]): a
//! non-match yields `None` (rendered as `PRELUDE_NO_MATCH_MARKER`) and never
//! affects the request's real captures. The supported query subset covers the
//! practical report cases (`status`, `header`, `body`, `jsonpath`, `regex`);
//! richer queries are a documented follow-up.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::environment::substitute;
use crate::hurl::HurlEntry;
use crate::hurl::{EntryOutcome, RunOutput};

use super::flow::{
    Binder, Element, EnvClause, FlowNode, ParallelSpec, Pattern, Producer, ReportFlow, ReportStmt,
    ResponseFmt, WithItem,
};
use super::model::{ReportResult, ReportRow};
use super::producers::{self, ProducerItem};

/// Engine defaults for the `PRELUDE_*` settings (overridable per flow/scope by a
/// `PRELUDE_*=…` assignment).
const DEFAULT_NO_MATCH: &str = "";
const DEFAULT_RESPONSE_FORMAT: &str = "pretty";
/// Default `PARALLEL` worker cap when a bare `PARALLEL` loop doesn't set one and
/// `PRELUDE_MAX_PARALLEL` is unset.
const DEFAULT_MAX_PARALLEL: usize = 8;

/// Abstraction over "send one resolved request and get its outcome", so the
/// interpreter can run against a fake in tests and the real pipeline in
/// production. Must be `Sync` so parallel loop workers (Phase 7.5) can share one
/// runner across threads.
pub trait EntryRunner: Sync {
    /// Send `base` with `vars` substituted, returning the raw run output. The
    /// interpreter only ever passes single-entry `base`s, so `entries` holds one
    /// [`EntryOutcome`] on success.
    fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput;
}

/// Production [`EntryRunner`]: routes each send through
/// [`crate::request::run_resolved_entry`] (base64 expansion → form-file staging
/// → content-length → `to_hurl` → `run_hurl`), rooted at `file_root` (the bound
/// collection's directory) so relative form-file paths resolve as expected.
pub struct LiveRunner {
    pub file_root: Option<PathBuf>,
}

impl EntryRunner for LiveRunner {
    fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
        crate::request::run_resolved_entry(base, vars, self.file_root.as_deref(), &[])
    }
}

/// A no-op [`EntryRunner`] for **dry runs**: it sends nothing and returns a
/// benign empty response for every request. Feeding it to [`run_flow`] exercises
/// the whole interpreter — producer expansion, loop nesting/products, ZIP/tuple
/// pairing, scoping and request-name resolution — so the caller learns the
/// projected row count, the resolved per-iteration variable bindings and any
/// producer/resolution problems, all without firing a single HTTP request.
pub struct DryRunner;

impl EntryRunner for DryRunner {
    fn run(&self, base: &HurlEntry, _vars: &HashMap<String, String>) -> RunOutput {
        RunOutput {
            entries: vec![EntryOutcome {
                method: base.method.clone(),
                url: base.url.clone(),
                status: 0,
                status_text: String::new(),
                headers: Vec::new(),
                body: String::new(),
                raw_body: String::new(),
                asserts: Vec::new(),
                captures: Vec::new(),
                duration_ms: 0,
                ok: true,
                error: None,
            }],
            error: None,
        }
    }
}

/// A row-lifecycle event delivered to a [`RowSink`] as a run progresses, so a
/// front-end can reflect each row's state (scheduled → running → finished) in a
/// pre-built grid. Both variants identify the target row by its structural
/// [`ReportRow::path`], which is stable and unique even under out-of-order
/// `PARALLEL` execution.
pub enum RowEvent<'r> {
    /// A leaf block (one with no nested loop, so it emits exactly one row) has
    /// begun running its requests. Fired once, carrying the row's `path`,
    /// *before* any of that row's requests are sent — the signal a front-end
    /// uses to mark the slot "running" while its (possibly slow) requests are in
    /// flight. Under `PARALLEL`, several rows can be running at once.
    Started(&'r [(usize, usize)]),
    /// A row has finished and is fully built (before any outer-scope broadcast
    /// cells or the final comparison/baseline collapse are applied); carries the
    /// row so the front-end can fill and un-grey its slot.
    Completed(&'r ReportRow),
}

/// A per-row streaming hook: called with a [`RowEvent`] as each row starts and
/// completes, so a front-end can fill a pre-built grid live as a long run
/// progresses (matched by [`ReportRow::path`], which is stable and unique)
/// instead of waiting for the whole run. Must be `Sync` — `PARALLEL` loop
/// workers call it from several threads at once (so a `mpsc::Sender` sink needs
/// a `Mutex`), and events may arrive out of iteration order under `PARALLEL`
/// (the `path` still identifies the target row).
pub type RowSink<'a> = dyn Fn(RowEvent) + Sync + 'a;

/// The immutable context a flow runs against: the bound collection's entries,
/// the base variable layer (global + pinned env, resolved once), any named
/// environments an `ENVS` loop may select, the report file's directory (for
/// resolving relative producer paths), and the runner.
pub struct RunContext<'a> {
    /// The bound collection's entries, resolved by title (see [`resolve_title`]).
    pub entries: &'a [HurlEntry],
    /// Global + pinned environment variables, already merged (pinned wins). The
    /// lowest layer of the variable precedence stack.
    pub base_vars: HashMap<String, String>,
    /// Environments selectable by name in a `FOR … IN ENVS` loop, each a flat
    /// `KEY → value` map. Empty when the flow has no `ENVS` loop.
    pub named_envs: HashMap<String, HashMap<String, String>>,
    /// Directory the report file lives in; relative producer paths (`FILES`,
    /// `FOLDERS`, `TUPLES FROM`) resolve against it (overridable via `# root:`).
    pub root: Option<PathBuf>,
    /// How each request is actually sent.
    pub runner: &'a dyn EntryRunner,
    /// Optional per-row streaming hook (see [`RowSink`]). `None` for a plain,
    /// collect-at-the-end run (CSV export, dry run, tests); `Some` when a
    /// front-end wants each row as it completes to fill a live grid.
    pub sink: Option<&'a RowSink<'a>>,
}

/// Resolve a request `name` against the bound collection's entries, mirroring
/// [`super::validate`]: exact full title → unique leaf (last `/`-segment) →
/// `None` (ambiguous or missing). Validation surfaces the ambiguous/missing
/// cases to the user before a run; at run time an unresolved name is recorded as
/// an error and the row still emitted.
pub fn resolve_title<'a>(entries: &'a [HurlEntry], name: &str) -> Option<&'a HurlEntry> {
    let exact: Vec<&HurlEntry> = entries.iter().filter(|e| e.title == name).collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if exact.len() > 1 {
        return None;
    }
    let leaves: Vec<&HurlEntry> = entries
        .iter()
        .filter(|e| e.title.rsplit('/').next() == Some(name))
        .collect();
    if leaves.len() == 1 {
        Some(leaves[0])
    } else {
        None
    }
}

/// Run a whole flow and collect its rows, applying the final comparison/baseline
/// collapse. This is the batch entry point (CSV export, dry run, tests); for a
/// live streaming run a front-end sets [`RunContext::sink`] and calls
/// [`run_flow_raw`] then [`finalize`] itself, so it can act on the pre-collapse
/// rows as they stream and swap in the finalized result at the end.
pub fn run_flow(flow: &ReportFlow, ctx: &RunContext) -> ReportResult {
    let mut result = run_flow_raw(flow, ctx);
    finalize(&mut result, flow, ctx);
    result
}

/// The **emit phase**: walk the flow and collect its rows exactly as produced —
/// one per innermost-loop iteration, in canonical order — *without* the
/// comparison/baseline collapse. Fires [`RunContext::sink`] once per row as it's
/// emitted (see [`RowSink`]). Separated from [`finalize`] so a streaming
/// front-end can build its grid from these raw rows (which map 1:1 to the sink's
/// updates) and only collapse at the end.
pub fn run_flow_raw(flow: &ReportFlow, ctx: &RunContext) -> ReportResult {
    let mut ex = Exec::new(ctx);
    let rows = ex.exec_block(&flow.nodes);
    // The table-wide no-match marker is the effective top-level
    // `PRELUDE_NO_MATCH_MARKER` (scoped assigns are popped after the run, so the
    // base frame holds the top-level value), defaulting to empty.
    let no_match_marker = ex
        .scopes
        .first()
        .and_then(|f| f.get("PRELUDE_NO_MATCH_MARKER"))
        .cloned()
        .unwrap_or_else(|| DEFAULT_NO_MATCH.to_string());
    ReportResult {
        rows,
        column_order: ex.column_order,
        no_match_marker,
        errors: ex.errors,
    }
}

/// The **finalize phase**: collapse baseline/candidate rows into a `Result` diff
/// when the flow configures an ENVS comparison or names a saved `# baseline:`
/// snapshot (a no-op otherwise). Done off the row model so the CSV writer and
/// the TUI grid both pick it up unchanged. Applied once, after the emit phase.
pub fn finalize(result: &mut ReportResult, flow: &ReportFlow, ctx: &RunContext) {
    if let Some(roles) = super::compare::comparison_roles(flow) {
        super::compare::apply(result, &roles);
    } else if let Some(rel) = flow
        .header
        .baseline()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        // No live ENVS comparison, but the report references a saved snapshot
        // (`# baseline:`): diff the run against it (PaperTrail "Source B"). The
        // path resolves like producer paths — relative to `# root:`/the report
        // dir. A missing/invalid snapshot is a non-fatal run error (rows still
        // produced, just without a `Result` verdict).
        let path = super::producers::resolve_path(ctx.root.as_deref(), rel);
        match super::baseline::Baseline::load(&path) {
            Ok(baseline) => super::baseline::apply(result, &baseline),
            Err(e) => result
                .errors
                .push(format!("baseline {}: {e}", path.display())),
        }
    }
}

/// Mutable interpreter state threaded through the walk.
struct Exec<'a> {
    ctx: &'a RunContext<'a>,
    /// Lexical scope stack (outer → inner). Each frame holds this-block's
    /// `Assign`/loop-bind variables; inner frames shadow outer.
    scopes: Vec<HashMap<String, String>>,
    /// Declared `LIST`s in scope (flat; list names are unique in practice).
    lists: HashMap<String, Producer>,
    /// Forward capture chain (values captured by requests, threaded to later
    /// requests). Highest precedence in [`Exec::vars_for`].
    captures: HashMap<String, String>,
    /// In-scope `FILES`/list loop-variable *values* in binding order — the row
    /// key (the `ENVS`/`TARGET` axis is deliberately excluded).
    key_parts: Vec<String>,
    /// The **structural path** to the current position: one `(node index in its
    /// block, iteration index)` pair per enclosing loop. Stable and unique per
    /// emitted row, and lexicographically ordered == canonical row order, so a
    /// streaming front-end can match a live row to its pre-built grid slot even
    /// when `PARALLEL` delivers rows out of order (see [`ReportRow::path`]).
    path: Vec<(usize, usize)>,
    /// The current `ENVS` target (environment name), if inside an `ENVS` loop.
    target: Option<String>,
    /// The current `ENVS` target's variables, layered above pinned/global.
    target_env: Option<HashMap<String, String>>,
    /// Cells produced by REPORT statements in *enclosing* blocks (before this
    /// loop) that broadcast into every row of this subtree. Threaded into each
    /// [`emit_row`](Self::emit_row) with `or_insert` semantics (a row's own
    /// inner cells win) so a streamed row already carries its outer-scope
    /// columns — without this the top-level `REPORT REQUEST` columns stay blank
    /// in the live grid until the run's final merge. Nearer scopes override
    /// farther ones.
    broadcast: HashMap<String, String>,
    /// Produced column keys in first-seen order (the default column order).
    column_order: Vec<String>,
    /// Non-fatal problems (unresolved request, transport failure, …). Every
    /// issue still leaves a row.
    errors: Vec<String>,
}

/// A cloneable snapshot of an [`Exec`]'s scope/capture/target state (no output
/// accumulators) — the seed each loop iteration forks from, so iterations are
/// independent (a requirement for `PARALLEL`, and applied to sequential loops
/// too for consistent semantics: a loop is a self-contained per-item chain and
/// its captures don't leak to the continuation).
#[derive(Clone)]
struct ExecState {
    scopes: Vec<HashMap<String, String>>,
    lists: HashMap<String, Producer>,
    captures: HashMap<String, String>,
    key_parts: Vec<String>,
    path: Vec<(usize, usize)>,
    target: Option<String>,
    target_env: Option<HashMap<String, String>>,
    broadcast: HashMap<String, String>,
}

/// The per-iteration output collected from a forked [`Exec`], reassembled in
/// iteration order after a (possibly parallel) loop.
struct IterOut {
    rows: Vec<ReportRow>,
    columns: Vec<String>,
    errors: Vec<String>,
}

impl<'a> Exec<'a> {
    fn new(ctx: &'a RunContext<'a>) -> Self {
        Exec {
            ctx,
            scopes: vec![HashMap::new()],
            lists: HashMap::new(),
            captures: HashMap::new(),
            key_parts: Vec::new(),
            path: Vec::new(),
            target: None,
            target_env: None,
            broadcast: HashMap::new(),
            column_order: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Snapshot the cloneable execution state (everything except the run-output
    /// accumulators). Used to fork independent iterations for a `PARALLEL` loop
    /// and to isolate each iteration's variable/capture scope.
    fn to_state(&self) -> ExecState {
        ExecState {
            scopes: self.scopes.clone(),
            lists: self.lists.clone(),
            captures: self.captures.clone(),
            key_parts: self.key_parts.clone(),
            path: self.path.clone(),
            target: self.target.clone(),
            target_env: self.target_env.clone(),
            broadcast: self.broadcast.clone(),
        }
    }

    /// Build a fresh [`Exec`] from a snapshot (with empty output accumulators) —
    /// the seed for one forked loop iteration.
    fn from_state(ctx: &'a RunContext<'a>, state: ExecState) -> Self {
        Exec {
            ctx,
            scopes: state.scopes,
            lists: state.lists,
            captures: state.captures,
            key_parts: state.key_parts,
            path: state.path,
            target: state.target,
            target_env: state.target_env,
            broadcast: state.broadcast,
            column_order: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// The effective `PARALLEL` worker count for a loop marked with `spec`:
    /// `PARALLEL(n)` → `n`; bare `PARALLEL` → `PRELUDE_MAX_PARALLEL` (default
    /// [`DEFAULT_MAX_PARALLEL`]). Clamped to `1..=count` (never more workers than
    /// iterations).
    fn parallel_degree(&self, spec: &ParallelSpec, count: usize) -> usize {
        let want = spec.degree.map(|d| d as usize).unwrap_or_else(|| {
            self.lookup("PRELUDE_MAX_PARALLEL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_PARALLEL)
        });
        want.clamp(1, count.max(1))
    }

    /// The variables visible for `columns:`/`REPORT (var)` — every layer except
    /// captures, low → high (base env, `ENVS` target env, then scope frames).
    fn visible_vars(&self) -> HashMap<String, String> {
        let mut m = self.ctx.base_vars.clone();
        if let Some(env) = &self.target_env {
            for (k, v) in env {
                m.insert(k.clone(), v.clone());
            }
        }
        for frame in &self.scopes {
            for (k, v) in frame {
                m.insert(k.clone(), v.clone());
            }
        }
        m
    }

    /// The full substitution map (precedence, low → high): global+pinned →
    /// `ENVS` env → flow assigns/loop binds → captures. Captures win, matching
    /// the resolved precedence in the design plan.
    fn vars_for(&self) -> HashMap<String, String> {
        let mut m = self.visible_vars();
        for (k, v) in &self.captures {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// Look up a single variable across the full precedence stack.
    fn lookup(&self, key: &str) -> Option<String> {
        if let Some(v) = self.captures.get(key) {
            return Some(v.clone());
        }
        for frame in self.scopes.iter().rev() {
            if let Some(v) = frame.get(key) {
                return Some(v.clone());
            }
        }
        if let Some(env) = &self.target_env
            && let Some(v) = env.get(key)
        {
            return Some(v.clone());
        }
        self.ctx.base_vars.get(key).cloned()
    }

    /// Set a variable in the current (innermost) scope frame.
    fn set_var(&mut self, key: &str, value: String) {
        self.scopes
            .last_mut()
            .expect("scope stack is never empty")
            .insert(key.to_string(), value);
    }

    /// The current effective default response format.
    fn default_response_fmt(&self) -> ResponseFmt {
        match self.lookup("PRELUDE_RESPONSE_FORMAT") {
            Some(v) if v.eq_ignore_ascii_case("raw") => ResponseFmt::Raw,
            _ => ResponseFmt::Pretty,
        }
    }

    fn note_column(&mut self, key: &str) {
        if !self.column_order.iter().any(|c| c == key) {
            self.column_order.push(key.to_string());
        }
    }

    /// Walk a block, returning the rows it produces. This-level `REPORT` cells
    /// are accumulated and either broadcast into the rows produced by nested
    /// loops or, when the block has no loop, emitted as a single row.
    fn exec_block(&mut self, nodes: &[FlowNode]) -> Vec<ReportRow> {
        // A block with no nested loop emits exactly one row (a "leaf" block).
        // Signal that row's slot as "running" up front — before any of its
        // requests are sent — so a streaming front-end shows it in flight.
        let is_leaf = !nodes
            .iter()
            .any(|n| matches!(n, FlowNode::ForEach { .. } | FlowNode::ForEnvs { .. }));
        if is_leaf && let Some(sink) = self.ctx.sink {
            sink(RowEvent::Started(&self.path));
        }
        let mut own: HashMap<String, String> = HashMap::new();
        let mut child_rows: Vec<ReportRow> = Vec::new();
        let mut has_loop = false;

        for (node_index, node) in nodes.iter().enumerate() {
            match node {
                FlowNode::Assign { key, value } => {
                    let v = substitute(&unquote(value), &self.vars_for());
                    self.set_var(key, v);
                }
                FlowNode::ListDecl { name, producer } => {
                    self.lists.insert(name.clone(), producer.clone());
                }
                FlowNode::Request { name } => {
                    self.run_request(name);
                }
                FlowNode::Report(stmt) => {
                    let cells = self.eval_report(stmt);
                    for (k, v) in cells {
                        self.note_column(&k);
                        own.insert(k, v);
                    }
                }
                FlowNode::ForEach {
                    pattern,
                    producer,
                    body,
                    parallel,
                } => {
                    has_loop = true;
                    child_rows.extend(self.run_for_each(
                        node_index,
                        pattern,
                        producer,
                        body,
                        parallel.as_ref(),
                        &own,
                    ));
                }
                FlowNode::ForEnvs {
                    var,
                    clause,
                    body,
                    parallel,
                } => {
                    has_loop = true;
                    child_rows.extend(self.run_for_envs(
                        node_index,
                        var,
                        clause,
                        body,
                        parallel.as_ref(),
                        &own,
                    ));
                }
            }
        }

        if has_loop {
            for row in &mut child_rows {
                for (k, v) in &own {
                    row.cells.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            child_rows
        } else {
            vec![self.emit_row(own)]
        }
    }

    /// Build the single row for a loop-free (innermost) block: this-block's
    /// cells plus a snapshot of the visible variables, the current row key, and
    /// the current `ENVS` target. Any enclosing-scope [`broadcast`](Self::broadcast)
    /// cells are folded in (a row's own inner cells win) so a *streamed* row
    /// already carries its outer-scope columns — the block's post-loop merge
    /// keeps the returned rows correct even for reports that follow the loop.
    /// Fires the streaming [`RowSink`] (if any) with the finished row before
    /// returning it, so a live front-end sees each row as it completes.
    fn emit_row(&self, mut cells: HashMap<String, String>) -> ReportRow {
        for (k, v) in &self.broadcast {
            cells.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let row = ReportRow {
            cells,
            vars: self.visible_vars(),
            key: self.key_parts.clone(),
            path: self.path.clone(),
            target: self.target.clone(),
        };
        if let Some(sink) = self.ctx.sink {
            sink(RowEvent::Completed(&row));
        }
        row
    }

    // --- requests & reports ------------------------------------------------

    /// Send a request by name (no column emitted), threading its captures
    /// forward. Records an error (but does not abort) if the name is unresolved
    /// or the send fails.
    fn run_request(&mut self, name: &str) -> Option<EntryOutcome> {
        let base = match resolve_title(self.ctx.entries, name) {
            Some(e) => e.clone(),
            None => {
                self.errors
                    .push(format!("request '{name}' could not be resolved"));
                return None;
            }
        };
        let vars = self.vars_for();
        let out = self.ctx.runner.run(&base, &vars);
        if let Some(err) = &out.error {
            self.errors.push(format!("{name}: {err}"));
        }
        let eo = out.entries.into_iter().next();
        if let Some(eo) = &eo {
            for (k, v) in &eo.captures {
                self.captures.insert(k.clone(), v.clone());
            }
        }
        eo
    }

    /// Evaluate a `REPORT` statement into cells for the current row. Cells are
    /// returned in a **stable order** (so the default column order is
    /// deterministic run-to-run — a `HashMap` here would randomise it).
    fn eval_report(&mut self, stmt: &ReportStmt) -> Vec<(String, String)> {
        match stmt {
            ReportStmt::Vars(vars) => vars
                .iter()
                .map(|v| (v.clone(), self.lookup(v).unwrap_or_default()))
                .collect(),
            ReportStmt::VarAs { var, name } => {
                vec![(name.clone(), self.lookup(var).unwrap_or_default())]
            }
            ReportStmt::Computed { template, name } => {
                let value = substitute(template, &self.vars_for());
                vec![(name.clone(), value)]
            }
            ReportStmt::Request {
                name,
                alias,
                response_fmt,
                show,
                hide,
                with,
            } => self.eval_report_request(name, alias.as_deref(), *response_fmt, show, hide, with),
        }
    }

    /// Run a `REPORT REQUEST`: send the request, thread its captures, and emit
    /// its intrinsic columns (`HttpStatus`/`Time`/`Asserts`/`Error`/`Response`)
    /// plus one column per `[Reports]`/`WITH` field, all namespaced by `alias`
    /// (default: the request's leaf name). Columns are emitted in a fixed order
    /// (intrinsics, then `[Reports]` fields, then `WITH` fields) so report output
    /// is deterministic.
    ///
    /// Column selection (applied after the full `cells` list is built):
    /// - A non-empty `show` keeps exactly those suffixes (in listed order) —
    ///   SHOW takes full precedence, including over the WITH suppression below.
    /// - Else if any `WITH` field declarations are present (`declared_only`),
    ///   the 5 intrinsics are suppressed so the report focuses on declared
    ///   fields. NOTE: a `[Reports]`-only request (no WITH fields) keeps its
    ///   intrinsics unchanged.
    /// - Otherwise all columns are kept (the unchanged default).
    /// - HIDE is then applied in all branches: any field whose suffix is in
    ///   `hide` is removed from the final output.
    fn eval_report_request(
        &mut self,
        name: &str,
        alias: Option<&str>,
        response_fmt: Option<ResponseFmt>,
        show: &[String],
        hide: &[String],
        with: &[WithItem],
    ) -> Vec<(String, String)> {
        let alias = alias
            .map(str::to_string)
            .unwrap_or_else(|| leaf(name).to_string());
        let mut cells: Vec<(String, String)> = Vec::new();

        let base = match resolve_title(self.ctx.entries, name) {
            Some(e) => e.clone(),
            None => {
                self.errors
                    .push(format!("request '{name}' could not be resolved"));
                cells.push((
                    format!("{alias}.Error"),
                    format!("unresolved request '{name}'"),
                ));
                return cells;
            }
        };

        let vars = self.vars_for();
        let out = self.ctx.runner.run(&base, &vars);
        let eo = match out.entries.into_iter().next() {
            Some(eo) => eo,
            None => {
                let err = out
                    .error
                    .unwrap_or_else(|| "request produced no response".into());
                self.errors.push(format!("{name}: {err}"));
                cells.push((format!("{alias}.Error"), err));
                return cells;
            }
        };

        // Thread real captures forward (report fields are evaluated separately
        // and never touch the capture chain).
        for (k, v) in &eo.captures {
            self.captures.insert(k.clone(), v.clone());
        }

        // Resolve the response format: per-statement / WITH override, else the
        // prelude default.
        let with_fmt = with.iter().find_map(|w| match w {
            WithItem::ResponseFmt(f) => Some(*f),
            _ => None,
        });
        let fmt = response_fmt
            .or(with_fmt)
            .unwrap_or_else(|| self.default_response_fmt());
        let response = match fmt {
            ResponseFmt::Raw => eo.raw_body.clone(),
            ResponseFmt::Pretty => eo.body.clone(),
        };

        // Intrinsics (fixed order).
        cells.push((format!("{alias}.HttpStatus"), eo.status.to_string()));
        cells.push((format!("{alias}.Time"), eo.duration_ms.to_string()));
        cells.push((format!("{alias}.Asserts"), asserts_summary(&eo)));
        cells.push((
            format!("{alias}.Error"),
            eo.error.clone().unwrap_or_default(),
        ));
        cells.push((format!("{alias}.Response"), response));

        // Report fields: the request's own `[Reports]` block, then `WITH` fields
        // (report-level overrides on name clash). Fields are stored raw (empty
        // for a non-match); the no-match marker is applied once, at render time.
        let mut fields: Vec<(String, String)> = base.reports.clone();
        for w in with {
            if let WithItem::Field { name, query } = w {
                fields.retain(|(n, _)| n != name);
                fields.push((name.clone(), query.clone()));
            }
        }
        for (fname, query) in fields {
            let value = eval_field(&query, &eo).unwrap_or_default();
            cells.push((format!("{alias}.{fname}"), value));
        }

        // Column selection is applied to the fully-built `cells` list.
        //
        // `declared_only`: true when this statement has at least one WITH field
        // declaration (not just a RESPONSE override). When true and SHOW is
        // absent, the 5 intrinsics are suppressed so the report focuses on the
        // explicitly declared fields. A `[Reports]`-only request (no WITH fields)
        // is unaffected — it still emits intrinsics by default.
        let declared_only = with.iter().any(|w| matches!(w, WithItem::Field { .. }));

        if !show.is_empty() {
            // SHOW takes full precedence: keep exactly the listed suffixes (in
            // listed order), including any intrinsics explicitly named.
            let mut kept: Vec<(String, String)> = Vec::with_capacity(show.len());
            for field in show {
                let key = format!("{alias}.{field}");
                if let Some((_, v)) = cells.iter().find(|(k, _)| *k == key) {
                    kept.push((key, v.clone()));
                }
            }
            cells = kept;
        } else if declared_only {
            // WITH fields were declared: suppress the 5 intrinsics so they don't
            // drown out the declared field columns. Identified by matching the
            // suffix after `alias.` against the known intrinsic names.
            cells.retain(|(k, _)| {
                let suffix = k.strip_prefix(&format!("{alias}.")).unwrap_or(k.as_str());
                !INTRINSIC_FIELDS.contains(&suffix)
            });
        }

        // Apply HIDE in all branches: remove any field whose suffix is in `hide`.
        if !hide.is_empty() {
            cells.retain(|(k, _)| {
                let suffix = k.strip_prefix(&format!("{alias}.")).unwrap_or(k.as_str());
                !hide.iter().any(|h| h == suffix)
            });
        }

        cells
    }

    // --- loops -------------------------------------------------------------

    /// Run a `FOR <pattern> IN <producer>` loop, returning all rows its
    /// iterations produce (always in producer order, even when parallel). Each
    /// item binds its positional values to the pattern (feeding the row key) and
    /// its named fields directly into scope; an arity mismatch is recorded but
    /// does not abort the run. Iterations are independent (captures do not leak
    /// between them or to the continuation).
    fn run_for_each(
        &mut self,
        node_index: usize,
        pattern: &Pattern,
        producer: &Producer,
        body: &[FlowNode],
        parallel: Option<&ParallelSpec>,
        inherited: &HashMap<String, String>,
    ) -> Vec<ReportRow> {
        let items = match self.expand_producer(producer) {
            Ok(t) => t,
            Err(e) => {
                self.errors.push(e);
                return Vec::new();
            }
        };
        // How one iteration seeds a fresh forked `Exec` and runs the body.
        let mut seed = self.to_state();
        // Fold this block's pre-loop REPORT cells into the broadcast set so each
        // streamed row carries them (nearer scope overrides farther).
        for (k, v) in inherited {
            seed.broadcast.insert(k.clone(), v.clone());
        }
        let ctx = self.ctx;
        let run_one = |i: usize| -> IterOut {
            let item = &items[i];
            let mut sub = Exec::from_state(ctx, seed.clone());
            sub.path.push((node_index, i));
            sub.check_arity(pattern, item);
            sub.scopes.push(HashMap::new());
            sub.bind_pattern(pattern, &item.values);
            for (k, v) in &item.named {
                sub.set_var(k, v.clone());
            }
            let rows = sub.exec_block(body);
            IterOut {
                rows,
                columns: sub.column_order,
                errors: sub.errors,
            }
        };
        self.run_iterations(items.len(), parallel, run_one)
    }

    /// Run a `FOR <var> IN ENVS <clause>` loop: swap the target-env layer per
    /// environment and run the body once each. `ENVS` is *not* part of the row
    /// key (it is the comparison axis); baseline envs run first. Iterations are
    /// independent and may run in parallel.
    fn run_for_envs(
        &mut self,
        node_index: usize,
        var: &str,
        clause: &EnvClause,
        body: &[FlowNode],
        parallel: Option<&ParallelSpec>,
        inherited: &HashMap<String, String>,
    ) -> Vec<ReportRow> {
        let names: Vec<String> = match clause {
            EnvClause::Plain(names) => names.clone(),
            EnvClause::Roles {
                baseline,
                comparisons,
                ..
            } => baseline.iter().chain(comparisons).cloned().collect(),
        };
        let mut seed = self.to_state();
        for (k, v) in inherited {
            seed.broadcast.insert(k.clone(), v.clone());
        }
        let ctx = self.ctx;
        let run_one = |i: usize| -> IterOut {
            let name = &names[i];
            let mut sub = Exec::from_state(ctx, seed.clone());
            sub.path.push((node_index, i));
            sub.target = Some(name.clone());
            sub.target_env = ctx.named_envs.get(name).cloned();
            if sub.target_env.is_none() {
                sub.errors
                    .push(format!("environment '{name}' is not loaded"));
            }
            sub.scopes.push(HashMap::new());
            sub.set_var(var, name.clone());
            let rows = sub.exec_block(body);
            IterOut {
                rows,
                columns: sub.column_order,
                errors: sub.errors,
            }
        };
        self.run_iterations(names.len(), parallel, run_one)
    }

    /// Execute `count` independent loop iterations — sequentially, or across a
    /// bounded thread pool when the loop is marked `PARALLEL` — and reassemble
    /// their output in iteration order (so a `PARALLEL` run is byte-identical to
    /// the sequential one, only faster). Merges each iteration's column-order
    /// notes and errors back into this `Exec`.
    fn run_iterations<F>(
        &mut self,
        count: usize,
        parallel: Option<&ParallelSpec>,
        run_one: F,
    ) -> Vec<ReportRow>
    where
        F: Fn(usize) -> IterOut + Sync,
    {
        let outs: Vec<IterOut> = match parallel {
            Some(spec) if count > 1 => {
                let degree = self.parallel_degree(spec, count);
                let next = AtomicUsize::new(0);
                let slots: Vec<Mutex<Option<IterOut>>> =
                    (0..count).map(|_| Mutex::new(None)).collect();
                std::thread::scope(|s| {
                    for _ in 0..degree {
                        s.spawn(|| {
                            loop {
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                if i >= count {
                                    break;
                                }
                                let out = run_one(i);
                                *slots[i].lock().unwrap() = Some(out);
                            }
                        });
                    }
                });
                slots
                    .into_iter()
                    .map(|m| m.into_inner().unwrap().expect("every slot is filled"))
                    .collect()
            }
            _ => (0..count).map(&run_one).collect(),
        };

        let mut rows = Vec::new();
        for out in outs {
            for c in &out.columns {
                self.note_column(c);
            }
            self.errors.extend(out.errors);
            rows.extend(out.rows);
        }
        rows
    }

    /// Record (but don't abort on) a destructuring arity mismatch: without a
    /// `...` rest, the pattern's positions must equal the item's values; with a
    /// rest, the pattern may bind fewer.
    fn check_arity(&mut self, pattern: &Pattern, item: &ProducerItem) {
        let want = pattern.binders.len();
        let got = item.values.len();
        let ok = if pattern.rest {
            want <= got
        } else {
            want == got
        };
        if !ok {
            self.errors.push(format!(
                "pattern binds {want} value(s) but the item has {got}"
            ));
        }
    }

    /// Bind one producer tuple to a destructuring pattern (introducing the named
    /// binders into the current scope and their values into the row key).
    fn bind_pattern(&mut self, pattern: &Pattern, tuple: &[String]) {
        for (i, binder) in pattern.binders.iter().enumerate() {
            let value = tuple.get(i).cloned().unwrap_or_default();
            if let Binder::Named(n) = binder {
                self.set_var(n, value.clone());
                self.key_parts.push(value);
            }
        }
    }

    /// Expand a producer into its items (positional values + named fields).
    /// `LIST` literals and named lists are pure; `FILES`/`FOLDERS`/`TUPLES`/`ZIP`
    /// touch the filesystem via [`super::producers`], resolving relative paths
    /// against the run root and substituting `{{var}}`s in paths/globs first.
    fn expand_producer(&self, producer: &Producer) -> Result<Vec<ProducerItem>, String> {
        let root = self.ctx.root.as_deref();
        match producer {
            Producer::List(elements) => Ok(elements
                .iter()
                .map(|el| match el {
                    Element::Scalar(s) => ProducerItem::scalar(self.subst_unquoted(s)),
                    Element::Tuple(parts) => ProducerItem {
                        values: parts.iter().map(|p| self.subst_unquoted(p)).collect(),
                        named: Vec::new(),
                    },
                })
                .collect()),
            Producer::Named(name) => {
                let inner = self
                    .lists
                    .get(name)
                    .ok_or_else(|| format!("list '{name}' is not declared"))?
                    .clone();
                self.expand_producer(&inner)
            }
            Producer::Files { dir, glob } => {
                let dir = producers::resolve_path(root, &self.subst_unquoted(dir));
                let glob = glob.as_ref().map(|g| self.subst_unquoted(g));
                Ok(producers::list_files(&dir, glob.as_deref())?
                    .into_iter()
                    .map(|p| ProducerItem::scalar(p.to_string_lossy().into_owned()))
                    .collect())
            }
            Producer::Folders { dir, roles } => {
                let dir = producers::resolve_path(root, &self.subst_unquoted(dir));
                let roles: Vec<(String, String)> = roles
                    .iter()
                    .map(|(r, g)| (r.clone(), self.subst_unquoted(g)))
                    .collect();
                let mut items = Vec::new();
                for folder in producers::list_folders(&dir)? {
                    let named = producers::folder_roles(&folder, &roles)?;
                    items.push(ProducerItem {
                        values: vec![folder.to_string_lossy().into_owned()],
                        named,
                    });
                }
                Ok(items)
            }
            Producer::Tuples { path } => {
                let path = producers::resolve_path(root, &self.subst_unquoted(path));
                producers::read_tuples(&path)
            }
            Producer::Zip(parts) => {
                let lists: Result<Vec<Vec<ProducerItem>>, String> =
                    parts.iter().map(|p| self.expand_producer(p)).collect();
                producers::zip_items(lists?)
            }
            Producer::Concat(parts) => {
                let lists: Result<Vec<Vec<ProducerItem>>, String> =
                    parts.iter().map(|p| self.expand_producer(p)).collect();
                producers::concat_items(lists?)
            }
        }
    }

    /// Substitute `{{var}}`s in `s` (after stripping a whole-string quote).
    fn subst_unquoted(&self, s: &str) -> String {
        substitute(&unquote(s), &self.vars_for())
    }
}

// --- helpers ---------------------------------------------------------------

/// The leaf (last `/`-segment) of a request title — the default alias.
fn leaf(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Strip one layer of surrounding double quotes if the whole string is quoted.
fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// A compact `passed/total` summary of an entry's asserts (empty when there are
/// none), surfaced as the `alias.Asserts` intrinsic column.
fn asserts_summary(eo: &EntryOutcome) -> String {
    let total = eo.asserts.len();
    if total == 0 {
        return String::new();
    }
    let passed = eo.asserts.iter().filter(|a| a.passed).count();
    format!("{passed}/{total}")
}

/// The intrinsic column suffixes every `REPORT REQUEST` emits (before any
/// `[Reports]`/`WITH` fields), in their fixed emission order. Shared so
/// validation of a `SHOW(...)` selector knows the always-present field names.
pub(crate) const INTRINSIC_FIELDS: [&str; 5] =
    ["HttpStatus", "Time", "Asserts", "Error", "Response"];

/// Evaluate one `[Reports]`/`WITH` field query against an already-received
/// response, *tolerantly*: a non-match (or an unsupported query type) returns
/// `None` (so the caller renders the no-match marker) rather than failing.
///
/// Supported query types (a practical subset of Hurl's grammar):
/// - `status` — the numeric HTTP status.
/// - `header "Name"` — the first matching response header (case-insensitive).
/// - `body` — the whole response body (as received).
/// - `jsonpath "$.a.b[0]"` — a dotted/indexed path into a JSON body.
/// - `regex "pat"` — first capture group (or whole match) against the body.
fn eval_field(query: &str, eo: &EntryOutcome) -> Option<String> {
    let query = query.trim();
    let (kind, rest) = match query.split_once(char::is_whitespace) {
        Some((k, r)) => (k, r.trim()),
        None => (query, ""),
    };
    match kind {
        "status" => Some(eo.status.to_string()),
        "body" => Some(eo.raw_body.clone()),
        "header" => {
            let name = string_arg(rest)?;
            eo.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&name))
                .map(|(_, v)| v.clone())
        }
        "jsonpath" => {
            let path = string_arg(rest)?;
            let root: serde_json::Value = serde_json::from_str(&eo.raw_body).ok()?;
            json_path_get(&root, &path).map(json_value_to_string)
        }
        "regex" => {
            let pat = string_arg(rest)?;
            let re = regex::Regex::new(&pat).ok()?;
            let caps = re.captures(&eo.raw_body)?;
            caps.get(1)
                .or_else(|| caps.get(0))
                .map(|m| m.as_str().to_string())
        }
        _ => None,
    }
}

/// The double-quoted string argument of a query (`header "X"` → `X`). Returns
/// `None` if the argument isn't a simple quoted string.
fn string_arg(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// Evaluate a minimal JSONPath (`$`, `.key`, `["key"]`/`['key']`, `[index]`)
/// against a JSON value. Deliberately small — it covers the paths report fields
/// actually use; wildcards/recursive-descent/filters are a documented follow-up.
fn json_path_get(root: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let rest = path.strip_prefix('$')?;
    let mut cur = root;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                let key = &rest[start..i];
                if key.is_empty() {
                    return None;
                }
                cur = cur.get(key)?;
            }
            b'[' => {
                let end = rest[i..].find(']')? + i;
                let inner = rest[i + 1..end].trim();
                cur = if let Some(k) = inner
                    .strip_prefix('"')
                    .and_then(|k| k.strip_suffix('"'))
                    .or_else(|| inner.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
                {
                    cur.get(k)?
                } else {
                    let idx: usize = inner.parse().ok()?;
                    cur.get(idx)?
                };
                i = end + 1;
            }
            _ => return None,
        }
    }
    Some(cur.clone())
}

/// Render a JSON value as a report cell: a string node yields its inner text
/// (no quotes); anything else its compact JSON form — never lossy.
fn json_value_to_string(v: serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hurl::AssertOutcome;
    use crate::report::parse_flow;
    use std::sync::Mutex;

    /// A canned response for a request title, built fresh into an
    /// [`EntryOutcome`] on each call (which isn't `Clone`).
    #[derive(Clone, Default)]
    struct Canned {
        status: u16,
        raw_body: String,
        pretty_body: String,
        captures: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        asserts: Vec<(bool,)>,
        duration_ms: u64,
        error: Option<String>,
    }

    /// A fake [`EntryRunner`] that records every `(title, vars)` it is asked to
    /// run and returns a per-title canned response, so the interpreter can be
    /// exercised with zero network. It also tracks peak concurrency (via
    /// `active`/`max_active`) so parallel execution can be observed, with an
    /// optional per-call `delay_ms` to widen the concurrency window.
    struct Fake {
        canned: HashMap<String, Canned>,
        calls: Mutex<Vec<(String, HashMap<String, String>)>>,
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay_ms: u64,
    }

    impl Fake {
        fn new(canned: &[(&str, Canned)]) -> Self {
            Fake {
                canned: canned
                    .iter()
                    .map(|(k, c)| (k.to_string(), c.clone()))
                    .collect(),
                calls: Mutex::new(Vec::new()),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                delay_ms: 0,
            }
        }
        /// Add a per-call delay so overlapping (parallel) calls are observable.
        fn with_delay(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
        fn call_vars(&self, title: &str) -> HashMap<String, String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .find(|(t, _)| t == title)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
        /// The highest number of concurrent `run` calls observed.
        fn peak_concurrency(&self) -> usize {
            self.max_active.load(Ordering::Relaxed)
        }
    }

    impl EntryRunner for Fake {
        fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            if self.delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            }
            self.calls
                .lock()
                .unwrap()
                .push((base.title.clone(), vars.clone()));
            let c = self.canned.get(&base.title).cloned().unwrap_or_default();
            let eo = EntryOutcome {
                method: base.method.clone(),
                url: base.url.clone(),
                status: c.status,
                status_text: String::new(),
                headers: c.headers,
                body: if c.pretty_body.is_empty() {
                    c.raw_body.clone()
                } else {
                    c.pretty_body
                },
                raw_body: c.raw_body,
                asserts: c
                    .asserts
                    .iter()
                    .map(|(p,)| AssertOutcome {
                        expr: String::new(),
                        passed: *p,
                        detail: String::new(),
                    })
                    .collect(),
                captures: c.captures,
                duration_ms: c.duration_ms,
                ok: c.error.is_none(),
                error: c.error.clone(),
            };
            self.active.fetch_sub(1, Ordering::SeqCst);
            RunOutput {
                entries: vec![eo],
                error: c.error,
            }
        }
    }

    /// Build an entry with the given title and optional `[Reports]` fields.
    fn entry(title: &str, reports: &[(&str, &str)]) -> HurlEntry {
        HurlEntry {
            title: title.to_string(),
            method: "GET".into(),
            url: "http://x".into(),
            reports: reports
                .iter()
                .map(|(n, q)| (n.to_string(), q.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn run(
        src: &str,
        entries: &[HurlEntry],
        base_vars: &[(&str, &str)],
        named_envs: &[(&str, &[(&str, &str)])],
        fake: &Fake,
    ) -> ReportResult {
        let flow = parse_flow(src).expect("flow parses");
        let ctx = RunContext {
            entries,
            base_vars: base_vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            named_envs: named_envs
                .iter()
                .map(|(name, kvs)| {
                    (
                        name.to_string(),
                        kvs.iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    )
                })
                .collect(),
            root: None,
            runner: fake,
            sink: None,
        };
        run_flow(&flow, &ctx)
    }

    #[test]
    fn linear_flow_emits_one_row_and_threads_captures() {
        let fake = Fake::new(&[
            (
                "Oauth",
                Canned {
                    status: 200,
                    captures: vec![("token".into(), "abc".into())],
                    ..Default::default()
                },
            ),
            (
                "me",
                Canned {
                    status: 200,
                    raw_body: "{\"name\":\"jo\"}".into(),
                    ..Default::default()
                },
            ),
        ]);
        let entries = [
            entry("Oauth", &[]),
            entry("me", &[("name", "jsonpath \"$.name\"")]),
        ];
        let res = run(
            "REQUEST Oauth\nREPORT REQUEST me\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 1, "loop-free flow = one row");
        // The captured `token` from Oauth must be visible to the `me` request.
        assert_eq!(fake.call_vars("me").get("token"), Some(&"abc".to_string()));
        assert_eq!(res.rows[0].cells.get("me.name"), Some(&"jo".to_string()));
        assert_eq!(
            res.rows[0].cells.get("me.HttpStatus"),
            Some(&"200".to_string())
        );
    }

    #[test]
    fn show_selector_prunes_columns_to_listed_fields() {
        // A heavy Response should be droppable while keeping small fields.
        let fake = Fake::new(&[(
            "me",
            Canned {
                status: 200,
                raw_body: "{\"name\":\"jo\",\"blob\":\"AAAAAAAA\"}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("me", &[("name", "jsonpath \"$.name\"")])];
        let res = run(
            "REPORT REQUEST me SHOW(name, HttpStatus)\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        let cells = &res.rows[0].cells;
        assert_eq!(cells.get("me.name"), Some(&"jo".to_string()));
        assert_eq!(cells.get("me.HttpStatus"), Some(&"200".to_string()));
        // The whole-body Response (and the other intrinsics) are pruned away.
        assert_eq!(cells.get("me.Response"), None);
        assert_eq!(cells.get("me.Time"), None);
        assert_eq!(cells.get("me.Asserts"), None);
        // Column order follows SHOW order, and only the two survive.
        assert_eq!(res.column_order, vec!["me.name", "me.HttpStatus"]);
    }

    #[test]
    fn show_selector_skips_a_field_the_request_never_produces() {
        let fake = Fake::new(&[(
            "me",
            Canned {
                status: 200,
                raw_body: "{}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("me", &[])];
        // `bogus` isn't an intrinsic or a field → simply absent (no empty cell).
        let res = run(
            "REPORT REQUEST me SHOW(HttpStatus, bogus)\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.column_order, vec!["me.HttpStatus"]);
        assert_eq!(res.rows[0].cells.get("me.bogus"), None);
    }

    #[test]
    fn assign_overrides_env_and_capture_overrides_assign() {
        // base env URL=base; flow assigns URL=flow -> the request sees flow.
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("send", &[])];
        run(
            "URL=flow\nREQUEST send\n",
            &entries,
            &[("URL", "base")],
            &[],
            &fake,
        );
        assert_eq!(fake.call_vars("send").get("URL"), Some(&"flow".to_string()));

        // Now a capture named URL should win over the assign.
        let fake2 = Fake::new(&[
            (
                "cap",
                Canned {
                    status: 200,
                    captures: vec![("URL".into(), "captured".into())],
                    ..Default::default()
                },
            ),
            (
                "send",
                Canned {
                    status: 200,
                    ..Default::default()
                },
            ),
        ]);
        let entries2 = [entry("cap", &[]), entry("send", &[])];
        run(
            "URL=flow\nREQUEST cap\nREQUEST send\n",
            &entries2,
            &[("URL", "base")],
            &[],
            &fake2,
        );
        assert_eq!(
            fake2.call_vars("send").get("URL"),
            Some(&"captured".to_string())
        );
    }

    #[test]
    fn report_request_emits_intrinsics_and_fields() {
        let fake = Fake::new(&[(
            "process",
            Canned {
                status: 201,
                raw_body: "{\"status\":\"ok\",\"n\":3}".into(),
                pretty_body: "{\n  \"status\": \"ok\"\n}".into(),
                asserts: vec![(true,), (true,), (false,)],
                duration_ms: 42,
                ..Default::default()
            },
        )]);
        let entries = [entry("process", &[("status", "jsonpath \"$.status\"")])];
        let res = run("REPORT REQUEST process\n", &entries, &[], &[], &fake);
        let cells = &res.rows[0].cells;
        assert_eq!(cells.get("process.HttpStatus"), Some(&"201".to_string()));
        assert_eq!(cells.get("process.Time"), Some(&"42".to_string()));
        assert_eq!(cells.get("process.Asserts"), Some(&"2/3".to_string()));
        assert_eq!(cells.get("process.status"), Some(&"ok".to_string()));
        // Default response format is pretty.
        assert_eq!(
            cells.get("process.Response"),
            Some(&"{\n  \"status\": \"ok\"\n}".to_string())
        );
    }

    #[test]
    fn missing_field_uses_no_match_marker() {
        let fake = Fake::new(&[(
            "process",
            Canned {
                status: 200,
                raw_body: "{\"a\":1}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("process", &[("missing", "jsonpath \"$.nope\"")])];
        let res = run(
            "PRELUDE_NO_MATCH_MARKER=\u{2205}\nREPORT REQUEST process\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        // The raw cell is empty; the marker is applied once, at render time.
        assert_eq!(res.no_match_marker, "\u{2205}");
        let col = crate::report::OutputColumn {
            header: "m".into(),
            sources: vec!["process.missing".into()],
        };
        assert_eq!(col.value(&res.rows[0], &res.no_match_marker), "\u{2205}");
    }

    #[test]
    fn report_vars_and_computed_columns() {
        let fake = Fake::new(&[]);
        let res = run(
            "FILE=a.jpg\nREPORT (FILE)\nREPORT FILE AS \"Pretty name\"\nREPORT \"doc-{{FILE}}\" AS label\n",
            &[],
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows[0].cells.get("FILE"), Some(&"a.jpg".to_string()));
        // `REPORT FILE AS "Pretty name"` puts the variable's value under the
        // renamed column.
        assert_eq!(
            res.rows[0].cells.get("Pretty name"),
            Some(&"a.jpg".to_string())
        );
        assert_eq!(
            res.rows[0].cells.get("label"),
            Some(&"doc-a.jpg".to_string())
        );
    }

    #[test]
    fn list_loop_emits_row_per_element_with_key() {
        let fake = Fake::new(&[]);
        let res = run(
            "LIST DOCS=[\"a\",\"b\",\"c\"]\nFOR X IN DOCS\n    REPORT (X)\nEND\n",
            &[],
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 3);
        let names: Vec<_> = res
            .rows
            .iter()
            .filter_map(|r| r.cells.get("X").cloned())
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        // Row key carries the loop var value.
        assert_eq!(res.rows[0].key, vec!["a".to_string()]);
    }

    #[test]
    fn tuple_list_loop_destructures_and_binds_both() {
        let fake = Fake::new(&[(
            "up",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("up", &[])];
        let res = run(
            "LIST DOCS=[(\"f1\",\"b1\"),(\"f2\",\"b2\")]\nFOR (FRONT, BACK) IN DOCS\n    REPORT REQUEST up\n    REPORT (FRONT, BACK)\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0].cells.get("FRONT"), Some(&"f1".to_string()));
        assert_eq!(res.rows[0].cells.get("BACK"), Some(&"b1".to_string()));
        assert_eq!(res.rows[0].key, vec!["f1".to_string(), "b1".to_string()]);
        // The request `up` ran once per iteration.
        assert_eq!(fake.call_count(), 2);
    }

    #[test]
    fn outer_report_broadcasts_into_every_loop_row() {
        let fake = Fake::new(&[
            (
                "oauth",
                Canned {
                    status: 200,
                    raw_body: "{}".into(),
                    ..Default::default()
                },
            ),
            (
                "up",
                Canned {
                    status: 200,
                    ..Default::default()
                },
            ),
        ]);
        let entries = [entry("oauth", &[]), entry("up", &[])];
        let res = run(
            "REPORT REQUEST oauth\nLIST DOCS=[\"a\",\"b\"]\nFOR X IN DOCS\n    REPORT REQUEST up\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 2);
        // oauth ran once, but its intrinsic column is on both rows.
        assert_eq!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(t, _)| t == "oauth")
                .count(),
            1
        );
        for row in &res.rows {
            assert!(row.cells.contains_key("oauth.HttpStatus"));
        }
    }

    /// Regression: an outer-scope `REPORT REQUEST` (before a loop) must appear on
    /// each row *as it streams*, not only in the run's final merged result — so a
    /// live grid shows the broadcast columns during a long run instead of leaving
    /// them blank until the very end.
    #[test]
    fn outer_report_columns_are_present_on_streamed_rows() {
        let fake = Fake::new(&[
            (
                "oauth",
                Canned {
                    status: 201,
                    ..Default::default()
                },
            ),
            (
                "up",
                Canned {
                    status: 200,
                    ..Default::default()
                },
            ),
        ]);
        let entries = [entry("oauth", &[]), entry("up", &[])];
        let flow = parse_flow(
            "REPORT REQUEST oauth\nFOR X IN [\"a\", \"b\"]\n    REPORT REQUEST up\nEND\n",
        )
        .unwrap();
        let streamed: Mutex<Vec<ReportRow>> = Mutex::new(Vec::new());
        let sink = |ev: RowEvent| {
            if let RowEvent::Completed(row) = ev {
                streamed.lock().unwrap().push(row.clone());
            }
        };
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: None,
            runner: &fake,
            sink: Some(&sink),
        };
        let result = run_flow_raw(&flow, &ctx);
        let streamed = streamed.into_inner().unwrap();

        assert_eq!(result.rows.len(), 2);
        assert_eq!(streamed.len(), 2, "one streamed row per loop iteration");
        // Every *streamed* row already carries the outer oauth column with its
        // real value — the fix for the broadcast-during-streaming bug.
        for row in &streamed {
            assert_eq!(
                row.cells.get("oauth.HttpStatus"),
                Some(&"201".to_string()),
                "streamed row is missing the broadcast outer-report column"
            );
        }
    }

    #[test]
    fn envs_loop_sets_target_and_layers_env_vars() {
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("send", &[])];
        let res = run(
            "FOR T IN ENVS \"au\", \"eu\"\n    REPORT REQUEST send\nEND\n",
            &entries,
            &[],
            &[("au", &[("REGION", "au-1")]), ("eu", &[("REGION", "eu-1")])],
            &fake,
        );
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0].target, Some("au".to_string()));
        assert_eq!(res.rows[1].target, Some("eu".to_string()));
        // ENVS is the comparison axis: not part of the row key.
        assert!(res.rows[0].key.is_empty());
        // The env's vars are visible in the row snapshot.
        assert_eq!(res.rows[0].vars.get("REGION"), Some(&"au-1".to_string()));
    }

    /// The streaming `RowSink` fires exactly once per emitted row, each row
    /// carries a unique structural `path`, and the sink's rows sorted by path
    /// reproduce the canonical (returned) row order — the contract the TUI relies
    /// on to fill a pre-built skeleton grid slot-by-slot as a run streams.
    #[test]
    fn streaming_sink_fires_once_per_row_with_unique_ordered_paths() {
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("send", &[])];
        let flow =
            parse_flow("FOR X IN [\"a\", \"b\", \"c\"]\n    REPORT REQUEST send\nEND\n").unwrap();
        let streamed: Mutex<Vec<ReportRow>> = Mutex::new(Vec::new());
        let sink = |ev: RowEvent| {
            if let RowEvent::Completed(row) = ev {
                streamed.lock().unwrap().push(row.clone());
            }
        };
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: None,
            runner: &fake,
            sink: Some(&sink),
        };
        let result = run_flow_raw(&flow, &ctx);
        let streamed = streamed.into_inner().unwrap();

        // One sink call per produced row.
        assert_eq!(result.rows.len(), 3);
        assert_eq!(streamed.len(), result.rows.len());

        // Paths are unique across the streamed rows.
        let mut paths: Vec<Vec<(usize, usize)>> = streamed.iter().map(|r| r.path.clone()).collect();
        let mut unique = paths.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), paths.len(), "streamed paths are unique");

        // Sorted paths reproduce the canonical returned-row order, and every
        // streamed path indexes into the returned rows (the skeleton match).
        paths.sort();
        let canonical: Vec<Vec<(usize, usize)>> =
            result.rows.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, canonical);
        let index: HashMap<Vec<(usize, usize)>, usize> = result
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.path.clone(), i))
            .collect();
        for row in &streamed {
            assert!(
                index.contains_key(&row.path),
                "every streamed row maps to a skeleton slot"
            );
        }
    }

    /// Every row is announced with a `Started` event carrying its structural
    /// path *before* it is `Completed`, so a front-end can mark a slot "running"
    /// while its requests are in flight, then "finished" when the row lands.
    #[test]
    fn streaming_sink_signals_started_before_completed_per_row() {
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("send", &[])];
        let flow =
            parse_flow("FOR X IN [\"a\", \"b\", \"c\"]\n    REPORT REQUEST send\nEND\n").unwrap();
        // Record the ordered (kind, path) event stream.
        #[derive(PartialEq, Debug)]
        enum Kind {
            Started,
            Completed,
        }
        let events: Mutex<Vec<(Kind, Vec<(usize, usize)>)>> = Mutex::new(Vec::new());
        let sink = |ev: RowEvent| {
            let mut log = events.lock().unwrap();
            match ev {
                RowEvent::Started(path) => log.push((Kind::Started, path.to_vec())),
                RowEvent::Completed(row) => log.push((Kind::Completed, row.path.clone())),
            }
        };
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: None,
            runner: &fake,
            sink: Some(&sink),
        };
        let result = run_flow_raw(&flow, &ctx);
        let events = events.into_inner().unwrap();

        // One Started and one Completed per row.
        assert_eq!(
            events.iter().filter(|(k, _)| *k == Kind::Started).count(),
            result.rows.len()
        );
        assert_eq!(
            events.iter().filter(|(k, _)| *k == Kind::Completed).count(),
            result.rows.len()
        );
        // For every row path, its Started appears before its Completed.
        for row in &result.rows {
            let started = events
                .iter()
                .position(|(k, p)| *k == Kind::Started && *p == row.path);
            let completed = events
                .iter()
                .position(|(k, p)| *k == Kind::Completed && *p == row.path);
            assert!(
                matches!((started, completed), (Some(s), Some(c)) if s < c),
                "row {:?} must be Started before Completed",
                row.path
            );
        }
    }

    #[test]
    fn with_field_overrides_reports_block() {
        let fake = Fake::new(&[(
            "p",
            Canned {
                status: 200,
                raw_body: "{\"a\":\"fromwith\",\"b\":\"orig\"}".into(),
                ..Default::default()
            },
        )]);
        // [Reports] declares a -> $.b (would be "orig"); WITH overrides a -> $.a.
        let entries = [entry("p", &[("a", "jsonpath \"$.b\"")])];
        let res = run(
            "REPORT REQUEST p WITH\n    a: jsonpath \"$.a\"\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows[0].cells.get("p.a"), Some(&"fromwith".to_string()));
    }

    #[test]
    fn response_raw_keeps_original_bytes() {
        let fake = Fake::new(&[(
            "p",
            Canned {
                status: 200,
                raw_body: "{\"z\":1,\"a\":2}".into(),
                pretty_body: "{\n  \"z\": 1,\n  \"a\": 2\n}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("p", &[])];
        let res = run("REPORT REQUEST p RESPONSE RAW\n", &entries, &[], &[], &fake);
        assert_eq!(
            res.rows[0].cells.get("p.Response"),
            Some(&"{\"z\":1,\"a\":2}".to_string())
        );
    }

    #[test]
    fn alias_renames_namespace() {
        let fake = Fake::new(&[(
            "process_file",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("process_file", &[])];
        let res = run(
            "REPORT REQUEST process_file AS proc\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert!(res.rows[0].cells.contains_key("proc.HttpStatus"));
        assert!(
            !res.rows[0]
                .cells
                .keys()
                .any(|k| k.starts_with("process_file."))
        );
    }

    #[test]
    fn spaced_request_name_alias_flows_into_columns() {
        // A request title with spaces must be quoted in the flow. Giving it a
        // space-free `AS` alias keeps the produced column keys — and the
        // `# columns:` references to them — clean identifiers.
        let fake = Fake::new(&[(
            "My Request",
            Canned {
                status: 201,
                raw_body: "hello".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("My Request", &[])];
        let src = "# columns: up.HttpStatus AS Status, up.Response AS Body\n\
                   REPORT REQUEST \"My Request\" AS up\n";
        let flow = parse_flow(src).expect("flow parses");
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: None,
            runner: &fake,
            sink: None,
        };
        let res = run_flow(&flow, &ctx);

        // The alias namespaces the cells; the spaced title never leaks into a key.
        let cells = &res.rows[0].cells;
        assert_eq!(cells.get("up.HttpStatus"), Some(&"201".to_string()));
        assert_eq!(cells.get("up.Response"), Some(&"hello".to_string()));
        assert!(!cells.keys().any(|k| k.starts_with("My Request.")));

        // `# columns:` resolves those alias keys into exactly two output columns.
        let cols = res.resolved_columns(&flow.header);
        let headers: Vec<&str> = cols.iter().map(|c| c.header.as_str()).collect();
        assert_eq!(headers, vec!["Status", "Body"]);
        assert_eq!(cols[0].value(&res.rows[0], "-"), "201");
        assert_eq!(cols[1].value(&res.rows[0], "-"), "hello");
    }

    #[test]
    fn jsonpath_supports_nested_and_index() {
        let fake = Fake::new(&[(
            "p",
            Canned {
                status: 200,
                raw_body: "{\"items\":[{\"name\":\"first\"},{\"name\":\"second\"}]}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("p", &[("n", "jsonpath \"$.items[1].name\"")])];
        let res = run("REPORT REQUEST p\n", &entries, &[], &[], &fake);
        assert_eq!(res.rows[0].cells.get("p.n"), Some(&"second".to_string()));
    }

    #[test]
    fn unresolved_request_records_error_but_still_emits_row() {
        let fake = Fake::new(&[]);
        let res = run("REPORT REQUEST ghost\n", &[], &[], &[], &fake);
        assert_eq!(res.rows.len(), 1);
        assert!(res.errors.iter().any(|e| e.contains("ghost")));
        assert!(res.rows[0].cells.contains_key("ghost.Error"));
    }

    #[test]
    fn resolve_title_prefers_exact_then_unique_leaf() {
        let entries = [entry("auth/Oauth", &[]), entry("upload/process_file", &[])];
        assert!(resolve_title(&entries, "auth/Oauth").is_some());
        assert!(resolve_title(&entries, "process_file").is_some());
        assert!(resolve_title(&entries, "missing").is_none());
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "paperboy_run_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// End-to-end: a `FILES` loop resolves paths against the run root, binds
    /// `{{FILE}}` and the row key, and runs the body once per matched file.
    #[test]
    fn files_loop_runs_body_per_file() {
        let d = tmpdir("files");
        std::fs::write(d.join("a.jpg"), "x").unwrap();
        std::fs::write(d.join("b.jpg"), "x").unwrap();
        std::fs::write(d.join("skip.png"), "x").unwrap();

        let fake = Fake::new(&[(
            "up",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("up", &[])];
        let flow = parse_flow(
            "FOR FILE IN FILES \".\" MATCH \"*.jpg\"\n    REPORT REQUEST up\n    REPORT (FILE)\nEND\n",
        )
        .unwrap();
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: Some(d.clone()),
            runner: &fake,
            sink: None,
        };
        let res = run_flow(&flow, &ctx);
        assert_eq!(res.rows.len(), 2, "one row per matched jpg");
        assert!(res.rows[0].cells.get("FILE").unwrap().ends_with("a.jpg"));
        assert_eq!(fake.call_count(), 2);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn nested_loops_produce_cartesian_product() {
        let fake = Fake::new(&[]);
        let res = run(
            "LIST A=[\"a1\",\"a2\"]\nLIST B=[\"b1\",\"b2\",\"b3\"]\nFOR X IN A\n    FOR Y IN B\n        REPORT (X, Y)\n    END\nEND\n",
            &[],
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 6, "2 x 3 = 6 rows");
        assert_eq!(res.rows[0].key, vec!["a1".to_string(), "b1".to_string()]);
        assert_eq!(res.rows[5].key, vec!["a2".to_string(), "b3".to_string()]);
    }

    #[test]
    fn arity_mismatch_is_recorded() {
        let fake = Fake::new(&[]);
        let res = run(
            "LIST DOCS=[(\"f1\",\"b1\")]\nFOR (A, B, C) IN DOCS\n    REPORT (A)\nEND\n",
            &[],
            &[],
            &[],
            &fake,
        );
        assert!(res.errors.iter().any(|e| e.contains("binds 3")));
    }

    #[test]
    fn envs_roles_merge_baseline_into_candidate_rows() {
        // A BASELINE/COMPARISON ENVS run collapses to one row per comparison
        // env: the baseline (`prod`) is consumed as the reference and each
        // candidate carries a `Result` (here `OK` — the empty responses match).
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )]);
        let entries = [entry("send", &[])];
        let res = run(
            "FOR TARGET IN ENVS BASELINE(\"prod\"), COMPARISON(\"stg1\", \"stg2\")\n    REPORT REQUEST send\n    REPORT (TARGET)\nEND\n",
            &entries,
            &[],
            &[("prod", &[]), ("stg1", &[]), ("stg2", &[])],
            &fake,
        );
        let targets: Vec<_> = res.rows.iter().filter_map(|r| r.target.clone()).collect();
        assert_eq!(
            targets,
            vec!["stg1", "stg2"],
            "baseline consumed; candidates remain"
        );
        assert!(
            res.rows
                .iter()
                .all(|r| r.cells.get(crate::report::compare::RESULT_COLUMN)
                    == Some(&crate::report::compare::MATCH.to_string()))
        );
        // All three envs still executed (baseline runs first, as the reference).
        assert_eq!(fake.call_count(), 3);
    }

    #[test]
    fn comparison_diffs_reported_field_across_envs() {
        // A per-env runner whose response echoes an env variable, so the reported
        // `overall` field genuinely differs between baseline and candidate.
        struct EchoEnv;
        impl EntryRunner for EchoEnv {
            fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
                let v = vars.get("VERDICT").cloned().unwrap_or_default();
                let body = format!("{{\"overall\":\"{v}\"}}");
                RunOutput {
                    entries: vec![EntryOutcome {
                        method: base.method.clone(),
                        url: base.url.clone(),
                        status: 200,
                        status_text: String::new(),
                        headers: Vec::new(),
                        body: body.clone(),
                        raw_body: body,
                        asserts: Vec::new(),
                        captures: Vec::new(),
                        duration_ms: 0,
                        ok: true,
                        error: None,
                    }],
                    error: None,
                }
            }
        }
        let entries = [entry("proc", &[])];
        let flow = parse_flow(
            "FOR TARGET IN ENVS BASELINE(\"prod\"), COMPARISON(\"staging\")\n    FOR FILE IN [\"a\", \"b\"]\n        REPORT REQUEST proc WITH\n            overall: jsonpath \"$.overall\"\n        END\n    END\nEND\n",
        )
        .unwrap();
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: [
                (
                    "prod".to_string(),
                    [("VERDICT".to_string(), "CLEAR".to_string())]
                        .into_iter()
                        .collect(),
                ),
                (
                    "staging".to_string(),
                    [("VERDICT".to_string(), "REVIEW".to_string())]
                        .into_iter()
                        .collect(),
                ),
            ]
            .into_iter()
            .collect(),
            root: None,
            runner: &EchoEnv,
            sink: None,
        };
        let res = run_flow(&flow, &ctx);

        // One candidate row per file (baseline consumed), showing candidate
        // values and the field-level diff.
        assert_eq!(res.rows.len(), 2);
        for r in &res.rows {
            assert_eq!(r.target.as_deref(), Some("staging"));
            assert_eq!(r.cells.get("proc.overall"), Some(&"REVIEW".to_string()));
            let result_cell = r
                .cells
                .get(crate::report::compare::RESULT_COLUMN)
                .expect("Result column");
            // Parse the JSON to verify structure.
            let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
            let obj = parsed.as_object().expect("object");
            assert!(obj.contains_key("prod (baseline)"));
            assert!(obj.contains_key("staging"));
            assert_eq!(obj["prod (baseline)"]["overall"], "CLEAR");
            assert_eq!(obj["staging"]["overall"], "REVIEW");
        }
        assert_eq!(
            res.column_order.first(),
            Some(&crate::report::compare::RESULT_COLUMN.to_string())
        );
    }

    #[test]
    fn baseline_directive_diffs_run_against_saved_snapshot() {
        use crate::report::baseline::Baseline;
        use crate::report::compare::RESULT_COLUMN;

        // A per-run runner whose reported `overall` field comes from a variable,
        // so we can save a first run as a snapshot then re-run with a different
        // value and see the `# baseline:` directive produce a `Result` diff.
        struct Echo;
        impl EntryRunner for Echo {
            fn run(&self, base: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
                let v = vars.get("VERDICT").cloned().unwrap_or_default();
                let body = format!("{{\"overall\":\"{v}\"}}");
                RunOutput {
                    entries: vec![EntryOutcome {
                        method: base.method.clone(),
                        url: base.url.clone(),
                        status: 200,
                        status_text: String::new(),
                        headers: Vec::new(),
                        body: body.clone(),
                        raw_body: body,
                        asserts: Vec::new(),
                        captures: Vec::new(),
                        duration_ms: 0,
                        ok: true,
                        error: None,
                    }],
                    error: None,
                }
            }
        }

        let dir = tmpdir("baseline");
        let entries = [entry("proc", &[])];
        let flow_src = "FOR FILE IN [\"a\", \"b\"]\n    REPORT REQUEST proc WITH\n        overall: jsonpath \"$.overall\"\n    END\nEND\n";
        let flow = parse_flow(flow_src).unwrap();

        // First run (VERDICT=CLEAR) → save as a `.baseline` snapshot.
        let ctx = RunContext {
            entries: &entries,
            base_vars: [("VERDICT".to_string(), "CLEAR".to_string())]
                .into_iter()
                .collect(),
            named_envs: HashMap::new(),
            root: Some(dir.clone()),
            runner: &Echo,
            sink: None,
        };
        let first = run_flow(&flow, &ctx);
        let snap_path = dir.join("proc.baseline");
        Baseline::from_result(&first).save(&snap_path).unwrap();

        // Second run (VERDICT=REVIEW) with a `# baseline:` directive pointing at
        // the snapshot — file "a" matches the key, "b" diffs the field.
        let flow2 = parse_flow(&format!("# baseline: proc.baseline\n{flow_src}")).unwrap();
        let ctx2 = RunContext {
            entries: &entries,
            base_vars: [("VERDICT".to_string(), "REVIEW".to_string())]
                .into_iter()
                .collect(),
            named_envs: HashMap::new(),
            root: Some(dir.clone()),
            runner: &Echo,
            sink: None,
        };
        let second = run_flow(&flow2, &ctx2);

        assert_eq!(
            second.column_order.first(),
            Some(&RESULT_COLUMN.to_string()),
            "Result column surfaced"
        );
        assert_eq!(second.rows.len(), 2);
        for r in &second.rows {
            let result_cell = r.cells.get(RESULT_COLUMN).expect("Result column");
            // Parse the JSON to verify structure.
            let parsed: serde_json::Value = serde_json::from_str(result_cell).expect("valid JSON");
            let obj = parsed.as_object().expect("object");
            assert!(obj.contains_key("baseline (baseline)"));
            assert!(obj.contains_key("comparison"));
            assert_eq!(
                obj["baseline (baseline)"]["overall"], "CLEAR",
                "every row differs from its snapshot sibling"
            );
            assert_eq!(
                obj["comparison"]["overall"], "REVIEW",
                "every row differs from its snapshot sibling"
            );
        }
        assert!(second.errors.is_empty(), "no baseline load error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn baseline_directive_matches_when_unchanged() {
        use crate::report::baseline::Baseline;
        use crate::report::compare::{MATCH, RESULT_COLUMN};

        let dir = tmpdir("baseline_match");
        let fake = Fake::new(&[(
            "proc",
            Canned {
                status: 200,
                raw_body: "{\"overall\":\"CLEAR\"}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("proc", &[])];
        let flow_src = "FOR FILE IN [\"a\"]\n    REPORT REQUEST proc WITH\n        overall: jsonpath \"$.overall\"\n    END\nEND\n";

        let flow = parse_flow(flow_src).unwrap();
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: Some(dir.clone()),
            runner: &fake,
            sink: None,
        };
        let first = run_flow(&flow, &ctx);
        let snap_path = dir.join("proc.baseline");
        Baseline::from_result(&first).save(&snap_path).unwrap();

        let flow2 = parse_flow(&format!("# baseline: proc.baseline\n{flow_src}")).unwrap();
        let second = run_flow(&flow2, &ctx);
        assert_eq!(second.rows.len(), 1);
        assert_eq!(
            second.rows[0].cells.get(RESULT_COLUMN),
            Some(&MATCH.to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn baseline_directive_missing_file_is_a_run_error() {
        let fake = Fake::new(&[(
            "proc",
            Canned {
                status: 200,
                raw_body: "{}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("proc", &[])];
        let flow = parse_flow("# baseline: nope.baseline\nREPORT REQUEST proc\n").unwrap();
        let ctx = RunContext {
            entries: &entries,
            base_vars: HashMap::new(),
            named_envs: HashMap::new(),
            root: Some(std::env::temp_dir()),
            runner: &fake,
            sink: None,
        };
        let res = run_flow(&flow, &ctx);
        assert_eq!(res.rows.len(), 1, "rows still produced");
        assert!(
            res.errors.iter().any(|e| e.starts_with("baseline ")),
            "missing snapshot recorded as a run error: {:?}",
            res.errors
        );
    }

    #[test]
    fn parallel_loop_matches_sequential_output() {
        // The same flow with and without `PARALLEL` must produce byte-identical
        // ordered rows — parallelism only changes *when* work happens.
        let canned = [(
            "up",
            Canned {
                status: 200,
                raw_body: "{}".into(),
                ..Default::default()
            },
        )];
        let entries = [entry("up", &[])];
        let body = "FOR X IN [\"a\",\"b\",\"c\",\"d\",\"e\"]\n    REPORT REQUEST up\n    REPORT (X)\nEND\n";

        let seq = Fake::new(&canned);
        let seq_res = run(body, &entries, &[], &[], &seq);
        let par = Fake::new(&canned);
        let par_res = run(&format!("PARALLEL {body}"), &entries, &[], &[], &par);

        let seq_x: Vec<_> = seq_res
            .rows
            .iter()
            .map(|r| r.cells.get("X").cloned())
            .collect();
        let par_x: Vec<_> = par_res
            .rows
            .iter()
            .map(|r| r.cells.get("X").cloned())
            .collect();
        assert_eq!(seq_x, par_x, "parallel output order matches sequential");
        assert_eq!(
            par_x,
            vec![
                Some("a".into()),
                Some("b".into()),
                Some("c".into()),
                Some("d".into()),
                Some("e".into())
            ]
        );
        assert_eq!(par_res.column_order, seq_res.column_order);
    }

    #[test]
    fn parallel_loop_actually_runs_concurrently() {
        // With a per-call delay, a `PARALLEL(4)` loop over 4 items must overlap;
        // the same flow run sequentially must never overlap.
        let canned = [(
            "up",
            Canned {
                status: 200,
                ..Default::default()
            },
        )];
        let entries = [entry("up", &[])];
        let body = "FOR X IN [\"a\",\"b\",\"c\",\"d\"]\n    REPORT REQUEST up\nEND\n";

        let par = Fake::new(&canned).with_delay(40);
        run(&format!("PARALLEL(4) {body}"), &entries, &[], &[], &par);
        assert!(par.peak_concurrency() >= 2, "parallel loop overlaps calls");

        let seq = Fake::new(&canned).with_delay(40);
        run(body, &entries, &[], &[], &seq);
        assert_eq!(seq.peak_concurrency(), 1, "sequential loop never overlaps");
    }

    #[test]
    fn parallel_degree_caps_concurrency() {
        // `PARALLEL(2)` over 6 slow items must never exceed two concurrent runs.
        let canned = [(
            "up",
            Canned {
                status: 200,
                ..Default::default()
            },
        )];
        let entries = [entry("up", &[])];
        let fake = Fake::new(&canned).with_delay(20);
        let res = run(
            "PARALLEL(2) FOR X IN [\"a\",\"b\",\"c\",\"d\",\"e\",\"f\"]\n    REPORT REQUEST up\n    REPORT (X)\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert_eq!(res.rows.len(), 6);
        assert!(
            fake.peak_concurrency() <= 2,
            "degree caps concurrency at 2, saw {}",
            fake.peak_concurrency()
        );
    }

    #[test]
    fn loop_captures_do_not_leak_to_continuation() {
        // A capture made inside a loop iteration must not be visible to a request
        // that runs after the loop (iterations are isolated snapshots).
        let fake = Fake::new(&[
            (
                "inside",
                Canned {
                    status: 200,
                    captures: vec![("secret".into(), "leaked".into())],
                    ..Default::default()
                },
            ),
            (
                "after",
                Canned {
                    status: 200,
                    ..Default::default()
                },
            ),
        ]);
        let entries = [entry("inside", &[]), entry("after", &[])];
        run(
            "FOR X IN [\"a\"]\n    REQUEST inside\nEND\nREQUEST after\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        assert!(
            !fake.call_vars("after").contains_key("secret"),
            "loop captures must not leak past END"
        );
    }

    #[test]
    fn parallel_envs_loop_preserves_role_order() {
        // `PARALLEL` on an ENVS comparison still merges deterministically:
        // the baseline is consumed and the candidates stay in clause order.
        let fake = Fake::new(&[(
            "send",
            Canned {
                status: 200,
                ..Default::default()
            },
        )])
        .with_delay(20);
        let entries = [entry("send", &[])];
        let res = run(
            "PARALLEL FOR TARGET IN ENVS BASELINE(\"prod\"), COMPARISON(\"stg1\", \"stg2\")\n    REPORT REQUEST send\n    REPORT (TARGET)\nEND\n",
            &entries,
            &[],
            &[("prod", &[]), ("stg1", &[]), ("stg2", &[])],
            &fake,
        );
        let targets: Vec<_> = res.rows.iter().filter_map(|r| r.target.clone()).collect();
        assert_eq!(targets, vec!["stg1", "stg2"]);
        assert!(
            fake.peak_concurrency() >= 2,
            "ENVS loop runs envs concurrently"
        );
    }

    #[test]
    fn with_fields_suppress_intrinsics_by_default() {
        // When a WITH field is declared, intrinsics are suppressed unless SHOW
        // explicitly names them. A [Reports] field is still emitted.
        let fake = Fake::new(&[(
            "svc",
            Canned {
                status: 200,
                raw_body: "{\"score\":42}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("svc", &[("score", "jsonpath \"$.score\"")])];
        let res = run(
            "REPORT REQUEST svc WITH\n    extra: jsonpath \"$.score\"\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        let cells = &res.rows[0].cells;
        // The WITH field and the [Reports] field are present.
        assert_eq!(cells.get("svc.extra"), Some(&"42".to_string()));
        assert_eq!(cells.get("svc.score"), Some(&"42".to_string()));
        // Intrinsics are suppressed because WITH fields were declared.
        assert_eq!(
            cells.get("svc.HttpStatus"),
            None,
            "intrinsics suppressed by WITH"
        );
        assert_eq!(cells.get("svc.Time"), None);
        assert_eq!(cells.get("svc.Response"), None);
    }

    #[test]
    fn reports_only_request_keeps_intrinsics() {
        // INTENDED ASYMMETRY: a request with only [Reports] fields (no WITH) keeps
        // intrinsics. WITH suppression only activates when WITH fields are present.
        let fake = Fake::new(&[(
            "svc",
            Canned {
                status: 200,
                raw_body: "{\"score\":42}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("svc", &[("score", "jsonpath \"$.score\"")])];
        let res = run("REPORT REQUEST svc\n", &entries, &[], &[], &fake);
        let cells = &res.rows[0].cells;
        assert_eq!(
            cells.get("svc.HttpStatus"),
            Some(&"200".to_string()),
            "intrinsics kept"
        );
        assert_eq!(cells.get("svc.score"), Some(&"42".to_string()));
    }

    #[test]
    fn show_wins_over_with_suppression_and_can_readd_intrinsic() {
        // When SHOW is present alongside WITH fields, SHOW takes full precedence
        // and can explicitly re-add an intrinsic that WITH would otherwise suppress.
        let fake = Fake::new(&[(
            "svc",
            Canned {
                status: 200,
                raw_body: "{\"score\":42}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("svc", &[])];
        let res = run(
            "REPORT REQUEST svc SHOW(HttpStatus, extra) WITH\n    extra: jsonpath \"$.score\"\nEND\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        let cells = &res.rows[0].cells;
        // SHOW re-adds HttpStatus even though WITH would normally suppress it.
        assert_eq!(
            cells.get("svc.HttpStatus"),
            Some(&"200".to_string()),
            "SHOW re-added intrinsic"
        );
        assert_eq!(cells.get("svc.extra"), Some(&"42".to_string()));
        // Other intrinsics not in SHOW are absent.
        assert_eq!(cells.get("svc.Response"), None);
        assert_eq!(res.column_order, vec!["svc.HttpStatus", "svc.extra"]);
    }

    #[test]
    fn hide_removes_named_field() {
        let fake = Fake::new(&[(
            "svc",
            Canned {
                status: 200,
                raw_body: "{\"score\":42}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("svc", &[("score", "jsonpath \"$.score\"")])];
        let res = run(
            "REPORT REQUEST svc HIDE(Response, Error)\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        let cells = &res.rows[0].cells;
        assert_eq!(cells.get("svc.Response"), None, "Response hidden");
        assert_eq!(cells.get("svc.Error"), None, "Error hidden");
        // Other intrinsics and [Reports] fields still present.
        assert_eq!(cells.get("svc.HttpStatus"), Some(&"200".to_string()));
        assert_eq!(cells.get("svc.score"), Some(&"42".to_string()));
    }

    #[test]
    fn hide_applied_after_show() {
        // HIDE acts after SHOW — even fields SHOW would keep can be removed by HIDE.
        let fake = Fake::new(&[(
            "svc",
            Canned {
                status: 200,
                raw_body: "{}".into(),
                ..Default::default()
            },
        )]);
        let entries = [entry("svc", &[])];
        let res = run(
            "REPORT REQUEST svc SHOW(HttpStatus, Time) HIDE(Time)\n",
            &entries,
            &[],
            &[],
            &fake,
        );
        let cells = &res.rows[0].cells;
        assert_eq!(cells.get("svc.HttpStatus"), Some(&"200".to_string()));
        assert_eq!(cells.get("svc.Time"), None, "HIDE removed Time after SHOW");
        assert_eq!(res.column_order, vec!["svc.HttpStatus"]);
    }
}
