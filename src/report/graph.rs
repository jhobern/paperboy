//! Building and ordering the dependency graph of a `GRAPH` region.
//!
//! A region is the author's assertion that the declared graph is complete.
//! This module takes the statements inside one and works out what that graph
//! actually is: which steps there are, which edges the data implies, and what
//! order satisfies them.
//!
//! Two orderings come out, and they answer different questions.
//!
//! [`Plan::order`] is what the sequential runner executes: repeatedly take the
//! **earliest-written ready step**. That rule is chosen so that a region with
//! no edges reproduces written order exactly — wrapping an existing block in
//! `GRAPH … END` has to be a no-op, or there is no incremental path onto the
//! feature.
//!
//! [`Plan::waves`] is what `--dry-run` prints: steps grouped by depth, so the
//! listing shows what *could* overlap rather than implying a total order that
//! doesn't exist. The two deliberately disagree — a step with no dependencies
//! written last is in wave 0 but runs last — because they are answering
//! "what constrains this?" and "what happens?" respectively.

use std::collections::{HashMap, HashSet};

use super::flow::{FlowNode, ReportFlow, ReportStmt, UsingItem};
use super::run::{HelperCollection, resolve_qualified};
use crate::hurl::HurlEntry;
use crate::i18n::{Strings, fill};

/// Why one step must run before another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    /// The dependent reads a value the dependency captures. Inferred, and named
    /// so `--dry-run` can show *which* value — that is what makes a missing or
    /// surprising edge visible by eye.
    Data(String),
    /// The author wrote `DEPENDS`. Kept distinct from an inferred edge in the
    /// dry-run listing, because the two answer different questions: an inferred
    /// edge can be checked against the collection, and a declared one can only
    /// be taken on trust — so the reader needs to know which they are looking
    /// at.
    Declared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

/// One statement inside a region.
#[derive(Debug, Clone)]
pub struct Step {
    /// The step name — `AS` if written, else the request's leaf name. The unit
    /// of identity: what an edge points at and what a row is keyed by.
    pub name: String,
    /// The request the step sends.
    pub request: String,
    /// Position among the region's statements, which is both the tie-break and
    /// the index back into the region body.
    pub written: usize,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub edges: Vec<Edge>,
    /// Execution order: earliest-written ready step, repeatedly.
    pub order: Vec<usize>,
    /// Depth grouping, for explaining the plan rather than running it.
    pub waves: Vec<Vec<usize>>,
}

impl Plan {
    /// The steps `idx` transitively depends on, in execution order.
    ///
    /// This is the visibility rule as well as the ordering one: inside a region
    /// a step is handed only its ancestors' captures, so a data edge nobody
    /// declared fails as an undefined variable instead of succeeding by
    /// accident and breaking the first time the scheduler reorders.
    pub fn ancestors(&self, idx: usize) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut stack = vec![idx];
        while let Some(n) = stack.pop() {
            for e in self.edges.iter().filter(|e| e.to == n) {
                if seen.insert(e.from) {
                    stack.push(e.from);
                }
            }
        }
        // Execution order, so that merging ancestors into the flat capture
        // chain reproduces the order they actually ran in — the flat map is
        // last-writer-wins, and "last" has to mean the same thing here as it
        // does outside a region.
        self.order
            .iter()
            .copied()
            .filter(|i| seen.contains(i))
            .collect()
    }

    /// The incoming edges of `idx`, in a stable order.
    pub fn incoming(&self, idx: usize) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to == idx).collect()
    }
}

/// The step name a node contributes, by the same rule the runner uses.
pub fn step_name(node: &FlowNode) -> Option<(String, &str)> {
    let (name, alias) = match node {
        FlowNode::Request { name, alias, .. } => (name, alias),
        FlowNode::Report(ReportStmt::Request { name, alias, .. }) => (name, alias),
        _ => return None,
    };
    let step = match alias {
        Some(a) => a.clone(),
        None => name.rsplit('/').next().unwrap_or(name).to_string(),
    };
    Some((step, name.as_str()))
}

/// The `DEPENDS` names on a node.
pub fn declared_deps(node: &FlowNode) -> &[String] {
    match node {
        FlowNode::Request { depends, .. } => depends,
        FlowNode::Report(ReportStmt::Request { depends, .. }) => depends,
        _ => &[],
    }
}

/// The request a step or cleanup will actually send: the collection entry with
/// its `USING(…)` overrides applied.
///
/// An override can both introduce a reference and take one away — `USING(url =
/// …)` replaces the URL wholesale — so reading the entry alone answers a
/// different question from the one the runner will ask. Applying them to a copy
/// asks exactly that question rather than keeping a second model of it that can
/// drift; the runner's own `apply_override` is used for the same reason. Its
/// errors are discarded because validation has already reported them, and on an
/// error the entry is left untouched.
fn effective_entry(
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    name: &str,
    using: &[UsingItem],
) -> Option<HurlEntry> {
    let mut entry = resolve_qualified(entries, helpers, name)?.clone();
    for item in using {
        if let UsingItem::Override { target, value } = item {
            let _ = crate::report::run::apply_override(&mut entry, target, value.clone());
        }
    }
    Some(entry)
}

/// The `USING(…)` values on a node — PaperTrail source text, so the only place
/// a qualified reference can be written.
fn using_values(node: &FlowNode) -> &[UsingItem] {
    match node {
        FlowNode::Request { using, .. } => using,
        FlowNode::Report(ReportStmt::Request { using, .. }) => using,
        FlowNode::Cleanup { using, .. } => using,
        _ => &[],
    }
}

/// Build the plan for one region body.
///
/// Returns every problem found rather than the first, because a region is
/// reviewed as a unit: an author who has just written twelve steps wants all
/// twelve verdicts, not a dozen rebuild cycles.
pub fn build(
    body: &[FlowNode],
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    strings: &Strings,
) -> Result<Plan, Vec<String>> {
    let mut steps = Vec::new();
    for (written, node) in body.iter().enumerate() {
        if let Some((name, request)) = step_name(node) {
            steps.push(Step {
                name,
                request: request.to_string(),
                written,
            });
        }
    }
    let by_name: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    // Which steps capture which names. A request's own `[Options] variable:`
    // defaults and `# [Gen]` rows are *not* captures: they exist before it
    // sends, so reading one implies nothing about ordering.
    let mut producers: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        let Some(entry) = resolve_qualified(entries, helpers, &step.request) else {
            continue; // unresolvable — reported elsewhere
        };
        for (cap, _) in &entry.captures {
            producers.entry(cap.clone()).or_default().push(i);
        }
    }

    let mut errors = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    // Declared edges first, so that when a step is named by both a `DEPENDS`
    // and a data reference the edge keeps the reason the author wrote down.
    // The pair says the same thing about order either way, and the explicit one
    // is the one they will be looking for in the listing.
    for (i, step) in steps.iter().enumerate() {
        for dep in declared_deps(&body[step.written]) {
            match by_name.get(dep.as_str()) {
                // A step depending on itself is never a typo worth guessing at:
                // it is a cycle of length one, and saying so here beats letting
                // the toposort report it as an unorderable region.
                Some(&j) if j == i => {
                    errors.push(fill(strings.diag_graph_depends_self, &[&step.name]))
                }
                Some(&j) => add_edge(&mut edges, j, i, EdgeKind::Declared),
                // `DEPENDS` names a *step*, and only a region's own steps are
                // ordered by the graph. A name from outside it is either a typo
                // or a misunderstanding of the barrier — everything before the
                // region has already finished — and both are worth saying.
                None => errors.push(fill(strings.diag_graph_depends_unknown, &[&step.name, dep])),
            }
        }
    }

    for (i, step) in steps.iter().enumerate() {
        let node = &body[step.written];
        // Everything the step reads: the request's own `{{VAR}}`s, plus the
        // flow's `USING(…)` values, which are substituted before the request is
        // built and so can carry a qualified name the request itself cannot.
        let mut refs: Vec<String> = Vec::new();
        if let Some(entry) = resolve_qualified(entries, helpers, &step.request) {
            let mut own: Vec<String> = crate::request::entry_referenced_keys(entry)
                .into_iter()
                .collect();
            // `entry_referenced_keys` hands back a `HashSet`; sort so a
            // region's edge list — and therefore its dry-run listing — is the
            // same from one run to the next.
            own.sort();
            refs.extend(own);
        }
        for item in using_values(node) {
            if let UsingItem::Override { value, .. } = item {
                refs.extend(crate::environment::referenced_keys(value));
            }
        }

        for r in refs {
            if let Some((qual, var)) = r.split_once('.') {
                // A qualified reference says which step it means, so there is
                // nothing to resolve and nothing to be ambiguous about. A name
                // that isn't a step in this region is left alone: validation
                // reports it against the whole lexical scope, which is wider
                // than one region.
                if let Some(&j) = by_name.get(qual)
                    && j != i
                {
                    add_edge(&mut edges, j, i, EdgeKind::Data(var.to_string()));
                }
                continue;
            }
            let Some(from) = producers.get(&r) else {
                continue; // produced outside the region, or not at all
            };
            let candidates: Vec<usize> = from.iter().copied().filter(|&j| j != i).collect();
            match candidates.len() {
                0 => {}
                1 => add_edge(&mut edges, candidates[0], i, EdgeKind::Data(r.clone())),
                n => {
                    // The whole point of the region is that ordering is
                    // explicit. A flat name with two producers has no ordering
                    // answer, and picking one would be inventing an edge the
                    // author never wrote.
                    let names: Vec<&str> =
                        candidates.iter().map(|&j| steps[j].name.as_str()).collect();
                    errors.push(fill(
                        strings.diag_graph_ambiguous_capture,
                        &[&step.name, &r, &n.to_string(), &names.join(", "), &r],
                    ));
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    match toposort(&steps, &edges) {
        Ok((order, waves)) => Ok(Plan {
            steps,
            edges,
            order,
            waves,
        }),
        Err(cycle) => {
            let names: Vec<&str> = cycle.iter().map(|&i| steps[i].name.as_str()).collect();
            Err(vec![fill(strings.diag_graph_cycle, &[&names.join(" → ")])])
        }
    }
}

fn add_edge(edges: &mut Vec<Edge>, from: usize, to: usize, kind: EdgeKind) {
    if !edges.iter().any(|e| e.from == from && e.to == to) {
        edges.push(Edge { from, to, kind });
    }
}

/// Execution order and wave grouping, or the steps left over when the graph
/// cannot be ordered at all.
type Sorted = (Vec<usize>, Vec<Vec<usize>>);

fn toposort(steps: &[Step], edges: &[Edge]) -> Result<Sorted, Vec<usize>> {
    let n = steps.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        preds[e.to].push(e.from);
    }

    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    // Earliest-written ready step, one at a time. Taking a whole ready *set*
    // per round would be simpler but wrong: with A(0) blocked on B(1) and an
    // unconstrained C(2), set-at-a-time yields B, C, A where written order
    // permits B, A, C — reordering a step nobody asked to reorder.
    while order.len() < n {
        let next = (0..n).find(|&i| !done[i] && preds[i].iter().all(|&p| done[p]));
        match next {
            Some(i) => {
                done[i] = true;
                order.push(i);
            }
            // Nothing is ready and nothing is finished: every remaining step is
            // waiting on another, which is a cycle by definition.
            None => return Err((0..n).filter(|&i| !done[i]).collect()),
        }
    }

    // Depth = one past the deepest predecessor. Computed over `order`, which is
    // already topological, so one pass suffices.
    let mut depth = vec![0usize; n];
    for &i in &order {
        depth[i] = preds[i].iter().map(|&p| depth[p] + 1).max().unwrap_or(0);
    }
    let mut waves: Vec<Vec<usize>> = vec![Vec::new(); depth.iter().copied().max().unwrap_or(0) + 1];
    if n > 0 {
        // Written order within a wave, so the listing reads down the file.
        let mut by_written: Vec<usize> = (0..n).collect();
        by_written.sort_by_key(|&i| steps[i].written);
        for i in by_written {
            waves[depth[i]].push(i);
        }
    } else {
        waves.clear();
    }

    Ok((order, waves))
}

/// The `--dry-run` wave listing for every region in `flow`.
///
/// Waves, not a numbered sequence: a numbered list would imply a total order
/// that does not exist, and the whole point of showing the plan is to show what
/// the graph does and does not constrain. Each step is annotated with why it
/// sits where it does, which is what makes a missing or surprising edge visible
/// by eye rather than by reading the flow and the collection side by side.
pub fn explain(
    flow: &ReportFlow,
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    strings: &Strings,
) -> Vec<String> {
    let mut out = Vec::new();
    for node in &flow.nodes {
        let FlowNode::Graph {
            name,
            body,
            parallel,
        } = node
        else {
            continue;
        };
        let title = match name {
            Some(n) => format!("GRAPH {n}"),
            None => "GRAPH".to_string(),
        };
        let concurrency = match parallel {
            Some(p) => match p.degree {
                Some(d) => format!(" · concurrency {d}"),
                None => " · concurrency default".to_string(),
            },
            None => String::new(),
        };
        let plan = match build(body, entries, helpers, strings) {
            Ok(p) => p,
            Err(errs) => {
                out.push(format!("{title} — cannot be ordered"));
                out.extend(errs.into_iter().map(|e| format!("  {e}")));
                continue;
            }
        };
        out.push(format!("{title} · {} steps{concurrency}", plan.steps.len()));
        for (w, wave) in plan.waves.iter().enumerate() {
            for (row, &idx) in wave.iter().enumerate() {
                let label = if row == 0 {
                    format!("wave {w}")
                } else {
                    String::new()
                };
                let mut why: Vec<String> = plan
                    .incoming(idx)
                    .iter()
                    .map(|e| match &e.kind {
                        EdgeKind::Data(var) => {
                            format!("data: {}.{var}", plan.steps[e.from].name)
                        }
                        EdgeKind::Declared => {
                            format!("declared: {}", plan.steps[e.from].name)
                        }
                    })
                    .collect();
                why.sort();
                let why = if why.is_empty() {
                    String::new()
                } else {
                    format!("   ({})", why.join(", "))
                };
                out.push(format!("  {label:<8} {:<34}{why}", plan.steps[idx].name));
            }
        }
        out.push("  within a wave, order and completion are not guaranteed".into());
    }
    out
}

/// The capture names produced *in this scope*: the steps written here and in
/// any region here, but not those inside a loop body.
///
/// A loop iteration runs on a fork whose captures are discarded at `END` — that
/// is what makes an iteration independent — so a name produced only inside a
/// loop is not available to anything after it. Counting one as still-produced
/// would keep a teardown that then reads a variable nobody in this run ever
/// set, which is the exact outcome pruning a stranded cleanup exists to avoid.
///
/// Cleanups count as producers here, even though [`step_name`] excludes them
/// (it answers a different question — which nodes are graph vertices). A
/// cleanup's capture is a real value that a sibling cleanup can read, and the
/// runner orders the two on exactly that basis; leaving them out made pruning
/// drop a teardown whose value was still being produced right beside it.
/// A request that has to be *told* a value is not the one that supplies it. A
/// capture is only counted here when the request making it does not also read
/// the same name, because such a request is waiting on the very value it
/// appears to offer: it would be answering `{{sid}}` out of its own response,
/// which is not a thing a request can do. Without that rule a cleanup reading
/// and capturing `sid` vouched for itself — and, by surviving, for every
/// sibling that read the same name — while a pair doing it to each other
/// vouched mutually and left the run reporting a teardown cycle nobody wrote.
/// That is a statement about a request and its own response, and nothing more:
/// where the name is *also* bound in scope — an assignment, a parameter,
/// another step's capture — a rotate-shaped request that reads the old value
/// and captures a new one produces it like anything else. Assignments and
/// parameters are counted for exactly that reason; they bind a name in this
/// scope as surely as a capture does.
///
/// `cleanups` says whether teardown captures count. Among siblings in one block
/// they do — the runner orders two cleanups against each other on exactly that
/// basis — but the answer is different one scope down, so the caller decides.
fn scope_captures(
    nodes: &[FlowNode],
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    cleanups: bool,
) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut visit = |nodes: &[FlowNode]| {
        for n in nodes {
            let request = match n {
                FlowNode::Assign { key, .. } => {
                    out.insert(key.clone());
                    continue;
                }
                FlowNode::Param(p) => {
                    out.insert(p.name.clone());
                    continue;
                }
                FlowNode::Cleanup { name, .. } => cleanups.then_some(name.as_str()),
                _ => step_name(n).map(|(_, r)| r),
            };
            // The self-read exclusion belongs to teardowns alone. A cleanup is
            // asking about its own dispatch moment, where a name it has to be
            // handed cannot also be one it supplies. An ordinary step captures
            // long before any teardown runs, so by the time a cleanup reads the
            // name it is bound — whatever it was worth beforehand, and wherever
            // that older value came from. Applying the rule to every node meant
            // the ordinary rotate shape (read the current `{{sid}}` from the
            // environment, capture the new one) produced nothing as far as
            // pruning could see, and the teardown was dropped in silence.
            let self_read_only = matches!(n, FlowNode::Cleanup { .. });
            if let Some(request) = request
                && let Some(e) = effective_entry(entries, helpers, request, using_values(n))
            {
                let reads = crate::request::entry_referenced_keys(&e);
                out.extend(
                    e.captures
                        .iter()
                        .map(|(c, _)| c)
                        .filter(|c| !self_read_only || !reads.contains(c.as_str()))
                        .cloned(),
                );
            }
        }
    };
    visit(nodes);
    for n in nodes {
        // A region is not a scope: its steps are named in the enclosing one and
        // its captures are merged back at the closing barrier.
        if let FlowNode::Graph { body, .. } = n {
            visit(body);
        }
    }
    out
}

/// Collect every `TRUTH` template the flow declares, wherever it is attached.
///
/// A truth is a real consumer of a step: `resolve_truths` builds its scope from
/// the row's cells first, and those are keyed `step.field`, so
/// `TRUTH "{{create.HttpStatus}}"` resolves. Validation must therefore *not*
/// refuse it — but pruning must not strand it either. Dropping the step removes
/// the cell, the placeholder survives substitution, and every row in the column
/// silently scores `Untested` with nothing said about why.
///
/// Asked of the flow rather than walked here, because a truth attaches at four
/// places — `REPORT "…" AS C`, `REPORT v AS C`, a `WITH` field, and the header's
/// `columns:` directive — and a walk that knew about three of them was exactly
/// the bug. `column_truths` is what the runner scores against, so asking it is
/// the same question, and the header is added because it is the one site the
/// node walk cannot reach.
///
/// A `columns:` directive *is* the resolved column set (`resolved_columns`):
/// the flow's own truths are merged into it by resolved header, and never over
/// an inline one. So when the directive is present the question is not "what
/// was written" but "what will be scored" — a flow truth for a column the
/// directive omits, or renames with `AS`, is dead text. Refusing a run over a
/// template that is never evaluated is the false-positive class two earlier
/// checks had to be withdrawn for, so the resolved question is asked here too
/// rather than half of it re-derived.
fn declared_truths(flow: &ReportFlow) -> Vec<String> {
    let mut from_flow = flow.column_truths();
    let mut out = Vec::new();
    if let Some(spec) = flow.header.columns() {
        for col in crate::report::model::parse_columns(spec) {
            if let Some(t) = col.truth.or_else(|| from_flow.remove(&col.header)) {
                out.push(t);
            }
        }
        return out;
    }
    // With no directive the columns are whatever the run produces, in
    // first-seen order — not knowable here, so every flow truth is a candidate.
    out.extend(from_flow.into_values());
    out
}

/// How many `CLEANUP`s the flow holds, at every depth.
fn count_cleanups(nodes: &[FlowNode]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            FlowNode::Cleanup { .. } => 1,
            FlowNode::ForEach { body, .. }
            | FlowNode::ForEnvs { body, .. }
            | FlowNode::Graph { body, .. } => count_cleanups(body),
            _ => 0,
        })
        .sum()
}

/// Drop the cleanups `keep` rejects, at every depth, telling it which capture
/// names are visible where each one is written.
fn retain_cleanups(
    nodes: &mut Vec<FlowNode>,
    visible: &HashSet<String>,
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    keep: &mut impl FnMut(&str, &str, &[String], &[UsingItem], &HashSet<String>) -> bool,
) {
    let mut here = visible.clone();
    here.extend(scope_captures(nodes, entries, helpers, true));
    nodes.retain(|n| match n {
        FlowNode::Cleanup {
            name,
            alias,
            depends,
            using,
        } => {
            let step = alias
                .clone()
                .unwrap_or_else(|| crate::report::run::leaf(name).to_string());
            keep(name, &step, depends, using, &here)
        }
        _ => true,
    });
    // A loop body is its own block: its cleanups run at the end of *every
    // iteration*, while this block's run once, after the whole loop is over. So
    // a teardown out here has not written anything yet when one in there is
    // dispatched, and cannot be what answers its reference — the body sees this
    // scope's steps and not its cleanups.
    //
    // And only what is written *above* the loop. The rest of this scope runs
    // after the body has finished, so a name bound down the page is not bound
    // yet on any iteration: counting it kept a teardown that then went out with
    // `{{sid}}` verbatim, once per item, with the run still reading as green.
    // A region is not a scope and keeps the lot.
    for i in 0..nodes.len() {
        let (above, rest) = nodes.split_at_mut(i);
        match &mut rest[0] {
            FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => {
                let mut body_visible = visible.clone();
                body_visible.extend(scope_captures(above, entries, helpers, false));
                retain_cleanups(body, &body_visible, entries, helpers, keep)
            }
            FlowNode::Graph { body, .. } => retain_cleanups(body, &here, entries, helpers, keep),
            _ => {}
        }
    }
}

/// Collect every reference that pruning has left with nothing to resolve to.
///
/// Only *qualified* references are checked, and deliberately so. A `step.var`
/// names its producer outright, so a dropped step makes it unresolvable no
/// matter where it is written or what else is in scope — the placeholder could
/// only reach the run verbatim.
///
/// A flat `{{sid}}` cannot be judged here at all. It is answered by whatever is
/// standing in the capture chain, and pruning has no idea what else could
/// answer it: the environment is not loaded at this point, and the chain is a
/// run-time thing with an order this walk does not have. Trying it anyway
/// refused working selections — a statement written *before* the region, which
/// could never have read the region's capture in the first place; a step whose
/// `USING(url = …)` had replaced the only text holding the reference; a name
/// the environment supplied all along. Refusing to run is only better than
/// running the wrong thing when it is actually the wrong thing.
fn scan_strands(nodes: &[FlowNode], dropped_names: &HashSet<String>, out: &mut Vec<String>) {
    for node in nodes {
        for text in crate::report::validate::interpolated_source(node) {
            for key in crate::environment::referenced_keys(text) {
                if let Some((step, _)) = key.split_once('.')
                    && dropped_names.contains(step)
                    && !out.contains(&key)
                {
                    out.push(key.clone());
                }
            }
        }
        match node {
            FlowNode::ForEach { body, .. }
            | FlowNode::ForEnvs { body, .. }
            | FlowNode::Graph { body, .. } => scan_strands(body, dropped_names, out),
            _ => {}
        }
    }
}

/// Prune every region in `flow` to the transitive closure of `targets`.
///
/// Pruning needs exactly the promise the region makes and nothing weaker: that
/// the declared graph is complete. Outside a region there is no such promise,
/// so a target naming a step out there is an error rather than a no-op — the
/// closure would be meaningless, and silently running the whole flow instead
/// would be the worst of the available answers.
pub fn prune_to_targets(
    flow: &mut ReportFlow,
    targets: &[String],
    entries: &[HurlEntry],
    helpers: &[HelperCollection],
    strings: &Strings,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    let mut matched: HashSet<String> = HashSet::new();
    // What pruning took away, so the cleanups can be pruned with it below.
    let mut dropped_names: HashSet<String> = HashSet::new();
    let mut dropped_captures: HashSet<String> = HashSet::new();
    for node in &mut flow.nodes {
        let FlowNode::Graph { body, .. } = node else {
            continue;
        };
        let plan = match build(body, entries, helpers, strings) {
            Ok(p) => p,
            Err(errs) => {
                errors.extend(errs);
                continue;
            }
        };
        let wanted: Vec<usize> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| targets.iter().any(|t| t == &s.name))
            .map(|(i, s)| {
                matched.insert(s.name.clone());
                i
            })
            .collect();
        // A region holding none of the targets contributes nothing to what was
        // asked for, so every step in it goes. Leaving it whole would mean
        // `--targets a` still sent every request of every *other* region —
        // which is the opposite of what naming a target is for, and dangerous
        // in the release-testing case the flag exists to serve.
        let mut keep_written: HashSet<usize> = HashSet::new();
        if !wanted.is_empty() {
            let mut keep: HashSet<usize> = wanted.iter().copied().collect();
            for &w in &wanted {
                keep.extend(plan.ancestors(w));
            }
            keep_written = keep.iter().map(|&i| plan.steps[i].written).collect();
        }
        // `Step::written` indexes `body` including its comments, so the filter
        // has to count the same way. Counting only steps made the two index
        // spaces drift apart the moment a comment appeared above a request, and
        // the wrong steps were dropped — silently, since the pruned flow is
        // never revalidated.
        for step in &plan.steps {
            let caps = resolve_qualified(entries, helpers, &step.request)
                .map(|e| {
                    e.captures
                        .iter()
                        .map(|(c, _)| c.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // What survives is not tracked here: `retain_cleanups` works it out
            // per scope, from the flow as it stands once every region has been
            // pruned, which is the only place that knows where a name can
            // actually be read.
            if !keep_written.contains(&step.written) {
                dropped_names.insert(step.name.clone());
                dropped_captures.extend(caps);
            }
        }
        let mut at = 0usize;
        body.retain(|n| {
            let idx = at;
            at += 1;
            if step_name(n).is_none() {
                return true; // comments carry through; they order nothing
            }
            keep_written.contains(&idx)
        });
    }
    // A cleanup undoes what a step did. If pruning removed every step it was
    // undoing, there is nothing left to tear down, and keeping it is worse than
    // useless: a declared dependency on a vanished step becomes a skip and an
    // exit code saying the run was incomplete, while an inferred one can send
    // the teardown with a variable nobody in this run ever set.
    //
    // Cleanups nested in a loop body count: the body runs once per item, and a
    // teardown inside it is no less stranded for being written there.
    //
    // "Still produced" is scope-aware: a step outside a region is never pruned,
    // so its captures count — but only where they can actually be read, which
    // for a step inside a loop body is that body alone.
    // Repeated to a fixed point. `scope_captures` counts a cleanup's captures
    // as produced — rightly, since the runner orders two cleanups against each
    // other on them — but the scope is built before the retain runs, so a
    // cleanup this very pass is about to drop could vouch for a sibling. The sibling survived on the strength of a name that, once the
    // pass finished, nothing in the run wrote, and went out with the
    // placeholder verbatim. Each round can only remove cleanups, so the loop
    // shrinks and terminates.
    let outer = HashSet::new();
    loop {
        let before = count_cleanups(&flow.nodes);
        retain_cleanups(
            &mut flow.nodes,
            &outer,
            entries,
            helpers,
            &mut |name, step, depends, using, visible| {
                // A cleanup dropped here is as gone as a pruned step, so it
                // joins them: the fixed-point loop then applies the same
                // doctrine transitively. Without it a teardown could survive
                // naming a sibling that no longer appears anywhere in the flow,
                // to be skipped at run time with a warning pointing at it — and
                // its own resource left standing.
                let decide = || -> bool {
                    if depends.iter().any(|d| dropped_names.contains(d.as_str())) {
                        return false;
                    }
                    let Some(effective) = effective_entry(entries, helpers, name, using) else {
                        return true;
                    };
                    // Only a name that *was* produced by a pruned step and is not
                    // produced by a surviving one in scope: anything else comes from the
                    // environment or from outside the region, and is none of pruning's
                    // business.
                    !crate::request::entry_referenced_keys(&effective)
                        .iter()
                        .any(|r| {
                            dropped_captures.contains(r.as_str()) && !visible.contains(r.as_str())
                        })
                };
                let keep = decide();
                if !keep {
                    dropped_names.insert(step.to_string());
                }
                keep
            },
        );
        if count_cleanups(&flow.nodes) == before {
            break;
        }
    }

    // Pruning removes steps, and what is left may still name one. A qualified
    // reference to a step that is no longer in the run cannot resolve: the
    // placeholder would be sent verbatim, or reported as a literal
    // `{{create.sid}}` in a column. The flow validated before pruning and is
    // never revalidated after it, so the one thing pruning can break is checked
    // here — and refused, because sending something other than what was asked
    // for is worse than not running.
    let mut stranded: Vec<String> = Vec::new();
    scan_strands(&flow.nodes, &dropped_names, &mut stranded);
    for text in declared_truths(flow) {
        for key in crate::environment::referenced_keys(&text) {
            if let Some((step, _)) = key.split_once('.')
                && dropped_names.contains(step)
                && !stranded.contains(&key)
            {
                stranded.push(key.clone());
            }
        }
    }
    for key in stranded {
        let step = key.split_once('.').map(|(s, _)| s).unwrap_or(&key);
        errors.push(fill(strings.diag_graph_target_strands, &[&key, step]));
    }

    // A target nobody matched is a typo or a step outside a region, and either
    // way running something other than what was asked for is worse than
    // refusing.
    for t in targets {
        if !matched.contains(t.as_str()) {
            errors.push(fill(strings.diag_graph_unknown_target, &[t]));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::parser::parse_flow;

    fn entry(title: &str, captures: &[&str], url_vars: &[&str]) -> HurlEntry {
        HurlEntry {
            title: title.into(),
            method: "GET".into(),
            url: format!(
                "http://x/{}",
                url_vars
                    .iter()
                    .map(|v| format!("{{{{{v}}}}}"))
                    .collect::<Vec<_>>()
                    .join("/")
            ),
            captures: captures
                .iter()
                .map(|c| ((*c).to_string(), "jsonpath \"$.t\"".to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_reference_only_an_assert_makes_still_orders_the_step() {
        // Hurl substitutes into an `[Asserts]` predicate exactly as it does into
        // a URL, so a step whose only use of a capture is there still depends on
        // whoever produced it. Missing that meant the consumer could be ordered
        // first and fail on an undefined variable.
        let mut consumer = entry("consumer", &[], &[]);
        consumer
            .asserts
            .push("jsonpath \"$.id\" == {{token}}".into());
        let entries = [consumer, entry("producer", &["token"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert_eq!(
            names(&p, &p.order),
            ["producer", "consumer"],
            "{:?}",
            p.order
        );
    }

    #[test]
    fn a_reference_only_an_option_makes_still_orders_the_step() {
        let mut consumer = entry("consumer", &[], &[]);
        consumer
            .options
            .push(crate::hurl::KvRow::new("retry", "{{attempts}}"));
        let entries = [consumer, entry("producer", &["attempts"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert_eq!(names(&p, &p.order), ["producer", "consumer"]);
    }

    #[test]
    fn a_generator_reading_a_capture_still_orders_the_step() {
        // A `# [Gen]` expression is not a template: a bare identifier is a
        // variable reference, resolved from the same map a `{{…}}` would be.
        // Scanning only the braces meant a step whose sole use of a capture was
        // inside a generator got no edge, and could run first.
        let mut consumer = entry("consumer", &[], &["sig"]);
        consumer
            .generators
            .push(("sig".into(), "sha256(token)".into()));
        let entries = [consumer, entry("producer", &["token"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert_eq!(names(&p, &p.order), ["producer", "consumer"]);
    }

    #[test]
    fn a_generators_own_row_and_its_functions_are_not_dependencies() {
        // `uuid` is a call and `seed` is declared by the block itself, so
        // neither reaches the variable map. An edge from either would order a
        // region by a name nothing outside it ever defines.
        let mut consumer = entry("consumer", &[], &[]);
        consumer.generators.push(("seed".into(), "uuid".into()));
        consumer
            .generators
            .push(("sig".into(), "sha256(seed)".into()));
        let entries = [consumer, entry("producer", &["seed"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert!(p.edges.is_empty(), "{:?}", p.edges);
    }

    #[test]
    fn a_placeholder_in_a_reports_field_does_not_invent_a_dependency() {
        // `[Reports]` looks like `[Captures]` but is PaperBoy's own metadata:
        // the query is handed to `eval_field` verbatim and nothing substitutes
        // into it. An edge drawn from it reordered the region, and made
        // `--targets` drag in a producer, for a field that still failed to
        // match.
        let mut consumer = entry("consumer", &[], &[]);
        consumer
            .reports
            .push(("selected".into(), "jsonpath \"{{path}}\"".into()));
        let entries = [consumer, entry("producer", &["path"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert!(p.edges.is_empty(), "{:?}", p.edges);
        assert_eq!(names(&p, &p.order), ["consumer", "producer"]);
    }

    #[test]
    fn a_disabled_row_does_not_invent_a_dependency() {
        // A disabled header never reaches the wire, so the placeholder in it is
        // not a use — and an edge drawn from text nothing evaluates would
        // reorder a region for no reason at all.
        let mut consumer = entry("consumer", &[], &[]);
        let mut row = crate::hurl::KvRow::new("X-Disabled", "{{token}}");
        row.enabled = false;
        consumer.headers.push(row);
        let entries = [consumer, entry("producer", &["token"], &[])];
        let p = plan(
            "GRAPH\n    REQUEST consumer\n    REQUEST producer\nEND\n",
            &entries,
        )
        .unwrap();
        assert!(p.edges.is_empty(), "{:?}", p.edges);
        assert_eq!(names(&p, &p.order), ["consumer", "producer"]);
    }

    /// Parse a flow whose first node is a region, and plan it.
    fn plan(src: &str, entries: &[HurlEntry]) -> Result<Plan, Vec<String>> {
        let flow = parse_flow(&format!("# collection: c\n\n{src}")).expect("parses");
        let FlowNode::Graph { body, .. } = &flow.nodes[0] else {
            panic!("expected a region, got {:?}", flow.nodes[0]);
        };
        build(body, entries, &[], &Strings::english())
    }

    fn names(p: &Plan, idxs: &[usize]) -> Vec<String> {
        idxs.iter().map(|&i| p.steps[i].name.clone()).collect()
    }

    #[test]
    fn a_region_with_no_edges_runs_in_written_order() {
        // The no-op guarantee: wrapping an existing sequential block in a
        // region must change nothing, or there is no way to adopt the feature
        // incrementally.
        let entries = [
            entry("a", &[], &[]),
            entry("b", &[], &[]),
            entry("c", &[], &[]),
        ];
        let p = plan(
            "GRAPH\n    REQUEST a\n    REQUEST b\n    REQUEST c\nEND\n",
            &entries,
        )
        .unwrap();
        assert!(p.edges.is_empty());
        assert_eq!(names(&p, &p.order), ["a", "b", "c"]);
        assert_eq!(p.waves.len(), 1, "no edges = one wave");
    }

    #[test]
    fn a_capture_someone_reads_is_an_edge() {
        let entries = [
            entry("login", &["token"], &[]),
            entry("api", &[], &["token"]),
        ];
        let p = plan("GRAPH\n    REQUEST login\n    REQUEST api\nEND\n", &entries).unwrap();
        assert_eq!(p.edges.len(), 1);
        assert_eq!(p.edges[0].kind, EdgeKind::Data("token".into()));
        assert_eq!(
            names(&p, &[p.edges[0].from, p.edges[0].to]),
            ["login", "api"]
        );
    }

    #[test]
    fn an_edge_may_point_forward_against_written_order() {
        // One of the three ways a region changes behaviour, and the reason the
        // ordering is computed rather than assumed.
        let entries = [
            entry("api", &[], &["token"]),
            entry("login", &["token"], &[]),
        ];
        let p = plan("GRAPH\n    REQUEST api\n    REQUEST login\nEND\n", &entries).unwrap();
        assert_eq!(names(&p, &p.order), ["login", "api"]);
    }

    #[test]
    fn a_cycle_is_an_error_naming_the_steps_in_it() {
        let entries = [entry("a", &["x"], &["y"]), entry("b", &["y"], &["x"])];
        let errs = plan("GRAPH\n    REQUEST a\n    REQUEST b\nEND\n", &entries).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("a") && errs[0].contains("b"), "{errs:?}");
    }

    #[test]
    fn a_flat_name_with_two_producers_in_the_region_is_ambiguous() {
        // Outside a region this resolves by last-writer-wins, which is exactly
        // the silent-wrong-value defect the feature exists to remove; inside
        // one there is no written order to fall back on.
        let entries = [
            entry("login", &["token"], &[]),
            entry("api", &[], &["token"]),
        ];
        let src = concat!(
            "GRAPH\n",
            "    REQUEST login AS first\n",
            "    REQUEST login AS second\n",
            "    REQUEST api\n",
            "END\n",
        );
        let errs = plan(src, &entries).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("token") && e.contains("first")),
            "{errs:?}"
        );
    }

    #[test]
    fn qualifying_the_reference_resolves_the_ambiguity() {
        let entries = [entry("login", &["token"], &[]), entry("api", &[], &[])];
        let src = concat!(
            "GRAPH\n",
            "    REQUEST login AS first\n",
            "    REQUEST login AS second\n",
            "    REQUEST api USING(header.X = \"{{second.token}}\")\n",
            "END\n",
        );
        let p = plan(src, &entries).unwrap();
        assert_eq!(p.edges.len(), 1);
        assert_eq!(names(&p, &[p.edges[0].from]), ["second"]);
    }

    #[test]
    fn waves_group_by_depth_while_order_stays_greedy() {
        // They answer different questions and are allowed to disagree: `c` is
        // unconstrained so it is in wave 0, but written last so it runs last.
        let entries = [
            entry("a", &["t"], &[]),
            entry("b", &[], &["t"]),
            entry("c", &[], &[]),
        ];
        let p = plan(
            "GRAPH\n    REQUEST a\n    REQUEST b\n    REQUEST c\nEND\n",
            &entries,
        )
        .unwrap();
        assert_eq!(names(&p, &p.order), ["a", "b", "c"]);
        assert_eq!(names(&p, &p.waves[0]), ["a", "c"]);
        assert_eq!(names(&p, &p.waves[1]), ["b"]);
    }

    #[test]
    fn a_step_does_not_depend_on_itself_for_what_it_captures() {
        // A request that both captures `token` and reads it (a refresh, say) is
        // not a cycle — it is one step.
        let entries = [entry("refresh", &["token"], &["token"])];
        let p = plan("GRAPH\n    REQUEST refresh\nEND\n", &entries).unwrap();
        assert!(p.edges.is_empty());
    }

    fn pruned(src: &str, targets: &[&str], entries: &[HurlEntry]) -> Result<String, Vec<String>> {
        let mut flow = parse_flow(&format!("# collection: c\n\n{src}")).expect("parses");
        let targets: Vec<String> = targets.iter().map(|t| (*t).to_string()).collect();
        prune_to_targets(&mut flow, &targets, entries, &[], &Strings::english())?;
        Ok(flow.to_text())
    }

    #[test]
    fn an_override_decides_whether_a_cleanup_is_stranded() {
        // Direction one: the override replaces the URL that held the reference,
        // so the request actually sent is clean and the teardown must survive —
        // dropping it leaks the resource silently.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("purge", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP purge USING(url = \"http://x/fixed\")\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP purge"), "{text}");

        // Direction two: the entry is clean but the override introduces the
        // stranded reference, so keeping it would put `{{sid}}` on the wire.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("purge", &[], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP purge USING(url = \"http://x/{{sid}}\")\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP purge"), "{text}");
    }

    #[test]
    fn a_cleanup_cannot_be_vouched_for_by_one_that_is_itself_dropped() {
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("rotate", &["sid"], &[]),
            entry("purge", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP rotate DEPENDS create\nCLEANUP purge\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP rotate"), "{text}");
        assert!(
            !text.contains("CLEANUP purge"),
            "purge was kept on a name only the dropped rotate wrote: {text}"
        );
    }

    #[test]
    fn a_truth_is_checked_wherever_it_is_attached() {
        let entries = [entry("create", &[], &[]), entry("target", &[], &[])];
        let body = "GRAPH\n    REPORT REQUEST create SHOW(HttpStatus)\n    REQUEST target\nEND\n";
        // A truth attaches at four places, and a walk that knew about one of
        // them left the other three stranding silently.
        for tail in [
            "REPORT V AS C TRUTH \"{{create.HttpStatus}}\"\n",
            "REPORT REQUEST target WITH\n    f: jsonpath \"$.x\" TRUTH \"{{create.HttpStatus}}\"\nEND\n",
        ] {
            let errs = pruned(&format!("{body}{tail}"), &["target"], &entries)
                .expect_err("a stranded TRUTH must be refused");
            assert!(
                errs.iter().any(|e| e.contains("create.HttpStatus")),
                "{tail} => {errs:?}"
            );
        }
        // The header's `columns:` directive is the site no node walk reaches.
        let mut flow = crate::report::parser::parse_flow(&format!(
            "# collection: c\n# columns: C TRUTH \"{{{{create.HttpStatus}}}}\"\n\n{body}"
        ))
        .expect("parses");
        let errs = prune_to_targets(
            &mut flow,
            &["target".to_string()],
            &entries,
            &[],
            &Strings::english(),
        )
        .expect_err("a stranded header TRUTH must be refused");
        assert!(
            errs.iter().any(|e| e.contains("create.HttpStatus")),
            "{errs:?}"
        );
    }

    #[test]
    fn a_request_that_reads_a_name_does_not_produce_it() {
        // Counting captures asked the wrong question. An entry that captures
        // `sid` twice contributed two producers by itself, and two cleanups
        // that each read and captured it vouched for each other — neither of
        // which can supply a value it is itself waiting for.
        let mut twice = entry("rotate", &["sid"], &["sid"]);
        twice
            .captures
            .push(("sid".into(), "jsonpath \"$.u\"".into()));
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            twice,
            entry("swap", &["sid"], &["sid"]),
            entry("purge", &[], &["sid"]),
        ];
        let region = "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n";
        for tail in [
            "CLEANUP rotate\n",
            "CLEANUP rotate\nCLEANUP swap\nCLEANUP purge\n",
        ] {
            let text = pruned(&format!("{region}{tail}"), &["target"], &entries).unwrap();
            assert!(
                !text.contains("CLEANUP"),
                "nothing left in the run writes sid: {tail} => {text}"
            );
        }
    }

    #[test]
    fn a_flow_truth_the_header_overrides_is_not_checked() {
        // `resolved_columns` never lets the flow's truth override an inline one
        // in the `columns:` directive, so the flow's is dead text — and
        // refusing a run over a template that is never evaluated is exactly the
        // false positive two withdrawn checks were built on.
        let entries = [entry("create", &[], &[]), entry("target", &[], &[])];
        let mut flow = crate::report::parser::parse_flow(
            "# collection: c\n# columns: C TRUTH \"{{target.HttpStatus}}\"\n\n\
             GRAPH\n    REPORT REQUEST create SHOW(HttpStatus)\n\
             \x20   REPORT REQUEST target SHOW(HttpStatus)\nEND\n\
             REPORT \"x\" AS C TRUTH \"{{create.HttpStatus}}\"\n",
        )
        .expect("parses");
        prune_to_targets(
            &mut flow,
            &["target".to_string()],
            &entries,
            &[],
            &Strings::english(),
        )
        .expect("the flow's truth is overridden and never evaluated");
    }

    #[test]
    fn an_ordinary_step_that_refreshes_a_name_produces_it_whatever_bound_it_first() {
        // The self-read exclusion belongs to teardowns alone. An ordinary step
        // captures long before any cleanup runs, so the name is bound by the
        // time one reads it — and where the *old* value came from is beside the
        // point. Applying the rule to every node meant the plain rotate shape,
        // reading the current `{{sid}}` out of the environment, produced
        // nothing as far as pruning could see and its teardown was dropped.
        let entries = [
            entry("provision", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("rotate", &["sid"], &["sid"]),
            entry("purge", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST provision\n    REQUEST target\nEND\n\
             REQUEST rotate\nCLEANUP purge\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(
            text.contains("CLEANUP purge"),
            "rotate runs and writes sid: {text}"
        );
    }

    #[test]
    fn a_binding_written_below_a_loop_does_not_vouch_for_a_cleanup_inside_it() {
        // A loop body's teardowns run at the end of every iteration, so nothing
        // written after the loop has happened yet on any of them. Counting the
        // whole enclosing scope regardless of position kept the cleanup, which
        // then went out with `{{sid}}` verbatim once per item.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("work", &[], &[]),
            entry("purge", &[], &["sid"]),
            entry("later", &["sid"], &[]),
        ];
        for tail in ["sid=later\n", "REQUEST later\n"] {
            let text = pruned(
                &format!(
                    "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
                     FOR x IN [\"1\"]\n    REQUEST work\n    CLEANUP purge\nEND\n{tail}"
                ),
                &["target"],
                &entries,
            )
            .unwrap();
            assert!(
                !text.contains("CLEANUP purge"),
                "{tail} is bound after the loop has finished: {text}"
            );
        }
    }

    #[test]
    fn a_cleanup_depending_on_a_dropped_cleanup_is_dropped_too() {
        // A cleanup pruning removes is as gone as a pruned step. Leaving it out
        // of the dropped set meant a teardown survived naming a sibling that no
        // longer appears anywhere in the flow — skipped at run time, with a
        // warning pointing at that vanished name, and its own resource left
        // standing.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("purge", &[], &["sid"]),
            entry("close", &[], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP purge\nCLEANUP close DEPENDS purge\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(
            !text.contains("CLEANUP"),
            "close depends on purge, which is gone: {text}"
        );
    }

    #[test]
    fn a_flow_truth_for_a_column_the_header_never_resolves_is_not_checked() {
        // A `columns:` directive *is* the resolved column set: a flow truth is
        // merged in only where its column appears there. So a truth for a
        // column the directive leaves out — or renames with `AS`, since the
        // merge is keyed by the resolved header — is never evaluated, and
        // refusing a run over it strands nothing.
        let entries = [entry("create", &[], &[]), entry("target", &[], &[])];
        for columns in ["D", "C AS Pretty"] {
            let mut flow = crate::report::parser::parse_flow(&format!(
                "# collection: c\n# columns: {columns}\n\n\
                 GRAPH\n    REPORT REQUEST create SHOW(HttpStatus)\n\
                 \x20   REPORT REQUEST target SHOW(HttpStatus)\nEND\n\
                 REPORT \"x\" AS C TRUTH \"{{{{create.HttpStatus}}}}\"\n"
            ))
            .expect("parses");
            prune_to_targets(
                &mut flow,
                &["target".to_string()],
                &entries,
                &[],
                &Strings::english(),
            )
            .unwrap_or_else(|e| panic!("columns: {columns} never resolves column C: {e:?}"));
        }
    }

    #[test]
    fn a_step_that_refreshes_a_name_it_was_given_still_produces_it() {
        // "Reads it, so doesn't produce it" is true of a request waiting on its
        // own response and of nothing else. A rotate reads the old value from
        // an assignment in scope and captures a new one — it genuinely writes
        // the name, and it sits outside every region, so it certainly runs.
        // Dropping the teardown that reads it leaks the resource in silence.
        let entries = [
            entry("provision", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("rotate", &["sid"], &["sid"]),
            entry("purge", &[], &["sid"]),
        ];
        let text = pruned(
            "sid=seed\nGRAPH\n    REQUEST provision\n    REQUEST target\nEND\n\
             REQUEST rotate\nCLEANUP purge\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(
            text.contains("CLEANUP purge"),
            "rotate runs and writes sid: {text}"
        );
    }

    #[test]
    fn a_cleanup_outside_a_loop_does_not_vouch_for_one_inside_it() {
        // A loop body is its own block: its cleanups run at the end of *each
        // iteration*, while the enclosing block's run once the whole loop is
        // over. So an outer teardown's capture has not happened yet when an
        // inner one is dispatched, and cannot be what answers its reference.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("work", &[], &[]),
            entry("purge", &[], &["sid"]),
            entry("rotate", &["sid"], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             FOR x IN [\"1\"]\n    REQUEST work\n    CLEANUP purge\nEND\n\
             CLEANUP rotate\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(
            !text.contains("CLEANUP purge"),
            "rotate runs after the loop, so nothing has written sid yet: {text}"
        );
    }

    #[test]
    fn a_cleanup_cannot_vouch_for_a_name_only_it_writes() {
        // A request cannot answer its own `{{sid}}` out of its own response, so
        // a cleanup that both reads and captures the name was keeping itself —
        // and propping up every sibling that read the same name.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("rotate", &["sid"], &["sid"]),
            entry("purge", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP rotate\nCLEANUP purge\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(
            !text.contains("CLEANUP rotate"),
            "rotate vouched for itself: {text}"
        );
        assert!(
            !text.contains("CLEANUP purge"),
            "purge was propped up by the self-vouching rotate: {text}"
        );
    }

    #[test]
    fn a_truth_template_naming_a_pruned_step_is_refused() {
        let entries = [entry("create", &[], &[]), entry("target", &[], &[])];
        let errs = pruned(
            "GRAPH\n    REPORT REQUEST create SHOW(HttpStatus)\n    REQUEST target\nEND\n\
             REPORT \"x\" AS C TRUTH \"{{create.HttpStatus}}\"\n",
            &["target"],
            &entries,
        )
        .expect_err("a stranded TRUTH must be refused");
        assert!(
            errs.iter().any(|e| e.contains("create.HttpStatus")),
            "{errs:?}"
        );
    }

    #[test]
    fn a_target_keeps_itself_and_everything_it_depends_on() {
        let entries = [
            entry("login", &["token"], &[]),
            entry("api", &[], &["token"]),
            entry("unrelated", &[], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST login\n    REQUEST api\n    REQUEST unrelated\nEND\n",
            &["api"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("REQUEST login"), "{text}");
        assert!(text.contains("REQUEST api"), "{text}");
        assert!(!text.contains("unrelated"), "{text}");
    }

    #[test]
    fn a_comment_above_a_step_does_not_shift_what_pruning_keeps() {
        // `Step::written` indexes the body including comments. A filter that
        // counted only steps drifted out of that index space at the first
        // comment and dropped the wrong ones — here, the producer the target
        // needs, leaving a target that cannot run.
        let entries = [
            entry("login", &["token"], &[]),
            entry("api", &[], &["token"]),
        ];
        let text = pruned(
            "GRAPH\n    # a comment shifts nothing\n    REQUEST login\n    REQUEST api\nEND\n",
            &["api"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("REQUEST login"), "{text}");
        assert!(text.contains("REQUEST api"), "{text}");
    }

    #[test]
    fn a_cleanup_goes_with_the_step_it_was_undoing() {
        // Keeping it would turn a targeted run into a skip and an exit code
        // saying the run was incomplete — when in fact nothing was left to tear
        // down, because the thing it tears down was never built.
        let entries = [
            entry("a", &[], &[]),
            entry("b", &[], &[]),
            entry("teardown", &[], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST a\n    REQUEST b\nEND\nCLEANUP teardown DEPENDS a\n",
            &["b"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP"), "{text}");
    }

    #[test]
    fn a_cleanup_whose_producer_survives_is_kept() {
        let entries = [
            entry("a", &[], &[]),
            entry("b", &[], &[]),
            entry("teardown", &[], &[]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST a\n    REQUEST b\nEND\nCLEANUP teardown DEPENDS b\n",
            &["b"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP teardown"), "{text}");
    }

    #[test]
    fn a_cleanup_that_reads_a_pruned_capture_goes_too() {
        // The inferred case, which is the worse one: kept, it would send the
        // teardown with a variable nobody in this run ever set.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("b", &[], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST b\nEND\nCLEANUP teardown\n",
            &["b"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP"), "{text}");
    }

    #[test]
    fn a_cleanup_keeps_a_capture_a_surviving_step_still_makes() {
        // The pruned region is not the only producer. A step outside any region
        // is never pruned, so the value the teardown reads is still there —
        // dropping it anyway leaked whatever that step created, which is the
        // one outcome a cleanup exists to prevent.
        let entries = [
            entry("outside", &["sid"], &[]),
            entry("discarded", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "REQUEST outside\n\
             GRAPH\n    REQUEST discarded\n    REQUEST target\nEND\n\
             CLEANUP teardown\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP teardown"), "{text}");
    }

    #[test]
    fn a_cleanup_keeps_a_capture_another_cleanup_still_makes() {
        // A cleanup is a producer too — a sibling reads its capture and the
        // runner orders the two on exactly that basis. Counting only ordinary
        // steps dropped the second teardown although the value it needed was
        // being minted right beside it.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("make", &["sid"], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             CLEANUP make\nCLEANUP teardown\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP teardown"), "{text}");
    }

    #[test]
    fn a_cleanup_in_a_region_sees_what_is_written_beside_the_region() {
        // The recursion handed a region body the *incoming* set, throwing away
        // everything written at the enclosing level, so a teardown inside one
        // could not see a producer standing right next to it.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("make", &["sid"], &[]),
            entry("target2", &[], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             REQUEST make\n\
             GRAPH\n    REQUEST target2\n    CLEANUP teardown\nEND\n",
            &["target", "target2"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP teardown"), "{text}");
    }

    #[test]
    fn a_capture_made_only_inside_a_loop_does_not_save_an_outer_cleanup() {
        // A loop iteration runs on a fork whose captures are discarded at END,
        // so `sid` never reaches the teardown written after the loop. Counting
        // it as still-produced kept a cleanup that would then be sent with the
        // literal `{{sid}}` — worse than dropping it, and the exact thing
        // pruning a stranded cleanup exists to avoid.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("make", &["sid"], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             FOR ITEM IN [1]\n    REQUEST make\nEND\n\
             CLEANUP teardown\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP"), "{text}");
    }

    #[test]
    fn a_cleanup_inside_the_loop_that_still_makes_its_value_is_kept() {
        // The other side of the scope rule: written inside the body, the
        // teardown can read what the body captured, so it has work to do.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("target", &[], &[]),
            entry("make", &["sid"], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST target\nEND\n\
             FOR ITEM IN [1]\n    REQUEST make\n    CLEANUP teardown\nEND\n",
            &["target"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("CLEANUP teardown"), "{text}");
    }

    #[test]
    fn pruning_refuses_to_strand_a_reference_written_in_a_list() {
        // A list literal's elements are interpolated like any other producer
        // text, so a reference there is as strandable as one in a column.
        let entries = [entry("create", &["sid"], &[]), entry("health", &[], &[])];
        let errs = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST health\nEND\n\
             FOR ITEM IN [\"{{create.sid}}\"]\n    REPORT ITEM AS S\nEND\n",
            &["health"],
            &entries,
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("create.sid")), "{errs:?}");
    }

    #[test]
    fn a_cleanup_inside_a_loop_is_pruned_like_any_other() {
        // A loop body runs once per item; a teardown written there is no less
        // stranded for being nested, and only the top level was being swept.
        let entries = [
            entry("create", &["sid"], &[]),
            entry("b", &[], &[]),
            entry("teardown", &[], &["sid"]),
        ];
        let text = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST b\nEND\n\
             FOR ITEM IN [1, 2]\n    CLEANUP teardown\nEND\n",
            &["b"],
            &entries,
        )
        .unwrap();
        assert!(!text.contains("CLEANUP"), "{text}");
    }

    #[test]
    fn pruning_refuses_to_strand_a_reference_to_the_step_it_removed() {
        // The flow validated before pruning and is never revalidated after it,
        // so a reference to a step the targets left out would reach the run as
        // a literal `{{create.sid}}` — reported in a column, or sent in a URL.
        let entries = [entry("create", &["sid"], &[]), entry("health", &[], &[])];
        let errs = pruned(
            "GRAPH\n    REQUEST create\n    REQUEST health\nEND\n\
             REPORT \"{{create.sid}}\" AS Session\n",
            &["health"],
            &entries,
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("create.sid")), "{errs:?}");
    }

    #[test]
    fn a_region_holding_no_target_is_emptied_not_left_whole() {
        // Naming a target must not still send every request of every other
        // region — that is the opposite of what the flag is for, and in the
        // release-testing case it is for, actively dangerous.
        let entries = [entry("a", &[], &[]), entry("b", &[], &[])];
        let text = pruned(
            "GRAPH first\n    REQUEST a\nEND\nGRAPH second\n    REQUEST b\nEND\n",
            &["a"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("REQUEST a"), "{text}");
        assert!(!text.contains("REQUEST b"), "{text}");
    }

    #[test]
    fn a_target_outside_any_region_is_refused() {
        // Only a region promises the complete graph a closure needs, so running
        // the whole flow instead would be answering a question nobody asked.
        let entries = [entry("login", &[], &[]), entry("api", &[], &[])];
        let errs = pruned(
            "REQUEST login\nGRAPH\n    REQUEST api\nEND\n",
            &["login"],
            &entries,
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.contains("login")), "{errs:?}");
    }

    #[test]
    fn pruning_leaves_statements_outside_the_region_alone() {
        // A region is one statement's worth of ordering to the code around it;
        // pruning inside it says nothing about the prelude that feeds it.
        let entries = [entry("setup", &[], &[]), entry("api", &[], &[])];
        let text = pruned(
            "REQUEST setup\nGRAPH\n    REQUEST api\nEND\n",
            &["api"],
            &entries,
        )
        .unwrap();
        assert!(text.contains("REQUEST setup"), "{text}");
    }

    #[test]
    fn the_wave_listing_names_the_edge_behind_each_step() {
        // Annotating *why* a step sits where it does is what makes a missing or
        // surprising edge visible by eye rather than by reading the flow and
        // the collection side by side.
        let entries = [
            entry("login", &["token"], &[]),
            entry("api", &[], &["token"]),
        ];
        let flow =
            parse_flow("# collection: c\n\nGRAPH\n    REQUEST login\n    REQUEST api\nEND\n")
                .expect("parses");
        let lines = explain(&flow, &entries, &[], &Strings::english());
        let text = lines.join("\n");
        assert!(text.contains("wave 0") && text.contains("login"), "{text}");
        assert!(text.contains("data: login.token"), "{text}");
        assert!(text.contains("not guaranteed"), "{text}");
    }

    #[test]
    fn ancestors_are_transitive_and_in_execution_order() {
        let entries = [
            entry("a", &["x"], &[]),
            entry("b", &["y"], &["x"]),
            entry("c", &[], &["y"]),
        ];
        let p = plan(
            "GRAPH\n    REQUEST a\n    REQUEST b\n    REQUEST c\nEND\n",
            &entries,
        )
        .unwrap();
        let c = p.steps.iter().position(|s| s.name == "c").unwrap();
        assert_eq!(names(&p, &p.ancestors(c)), ["a", "b"]);
    }
}
