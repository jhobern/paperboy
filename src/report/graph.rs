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

/// The `USING(…)` values on a node — PaperTrail source text, so the only place
/// a qualified reference can be written.
fn using_values(node: &FlowNode) -> &[UsingItem] {
    match node {
        FlowNode::Request { using, .. } => using,
        FlowNode::Report(ReportStmt::Request { using, .. }) => using,
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
        if wanted.is_empty() {
            continue;
        }
        let mut keep: HashSet<usize> = wanted.iter().copied().collect();
        for &w in &wanted {
            keep.extend(plan.ancestors(w));
        }
        let keep_written: HashSet<usize> = keep.iter().map(|&i| plan.steps[i].written).collect();
        let mut written = 0usize;
        body.retain(|n| {
            if step_name(n).is_none() {
                return true; // comments carry through; they order nothing
            }
            let w = written;
            written += 1;
            keep_written.contains(&w)
        });
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
