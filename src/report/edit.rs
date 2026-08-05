//! Front-end-agnostic **structural editing** of a PaperTrail [`ReportFlow`] —
//! the operations behind the "Scratch-like" node/block editors in both the
//! terminal UI and the GUI.
//!
//! A report flow is a linear/nested outline of statements, so authoring it is a
//! matter of inserting / removing / moving / replacing whole *nodes* rather than
//! typing text. These helpers flatten the AST into a navigable list of rows and
//! mutate it by *path* (a sequence of indices, each stepping into a loop body),
//! keeping the tree the single source of truth: every structural edit
//! re-serializes back to `report.text` via [`ReportFlow::to_text`], so the text
//! and node views round-trip and both front-ends share this one implementation.

use crate::i18n::Strings;
use crate::report::flow::{
    EnvClause, FlowNode, ParallelSpec, Pattern, Producer, ReportFlow, ReportStmt, RoleRef, WithItem,
};
use crate::report::parse_flow;

// ---------------------------------------------------------------------------
// The flattened, navigable outline
// ---------------------------------------------------------------------------

/// One displayed row of the node outline. A leaf statement is one row; a `FOR`
/// loop is a header row (`kind = LoopHead`) whose body nests one level deeper
/// and closes with a synthetic `END` row (`kind = LoopEnd`). Row 0 is always
/// the synthetic `Begin` root.
pub(crate) struct NodeRow {
    /// Indentation depth (0 = top level; the `Begin` root is also 0).
    pub(crate) depth: usize,
    /// The rendered label (the node's [`FlowNode::label`]; `""` for the
    /// synthetic rows, which get their text from `kind`).
    pub(crate) label: String,
    pub(crate) kind: RowKind,
    /// Path to the addressed AST node: a sequence of indices, each stepping
    /// into a loop body. Empty for `Begin`; for `LoopEnd` it is the `FOR`
    /// node's own path (same as its `LoopHead`).
    pub(crate) path: Vec<usize>,
    /// For a `REQUEST` / `REPORT REQUEST` row, whether the referenced request
    /// name resolves in the bound collection (green) or not (amber). `None`
    /// for every other row.
    pub(crate) req_ok: Option<bool>,
}

/// The role of a [`NodeRow`] — drives rendering and where an insert lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowKind {
    /// The synthetic root; inserting here adds the first top-level node.
    Begin,
    /// A leaf statement (assignment, request, report, list).
    Leaf,
    /// The `FOR … IN …` opener of a loop.
    LoopHead,
    /// The synthetic `END` closing a loop.
    LoopEnd,
}

/// Flatten a flow into the display rows, tagging request rows with whether they
/// resolve (via `resolves`). Row 0 is the `Begin` root.
pub(crate) fn flatten(flow: &ReportFlow, resolves: &impl Fn(&str) -> bool) -> Vec<NodeRow> {
    let mut rows = vec![NodeRow {
        depth: 0,
        label: String::new(),
        kind: RowKind::Begin,
        path: Vec::new(),
        req_ok: None,
    }];
    let mut prefix = Vec::new();
    push_nodes(&flow.nodes, &mut prefix, 1, resolves, &mut rows);
    rows
}

fn push_nodes(
    nodes: &[FlowNode],
    prefix: &mut Vec<usize>,
    depth: usize,
    resolves: &impl Fn(&str) -> bool,
    rows: &mut Vec<NodeRow>,
) {
    for (i, node) in nodes.iter().enumerate() {
        prefix.push(i);
        let req_ok = node.request_name().map(resolves);
        if let Some(body) = loop_body(node) {
            rows.push(NodeRow {
                depth,
                label: node.label(),
                kind: RowKind::LoopHead,
                path: prefix.clone(),
                req_ok,
            });
            push_nodes(body, prefix, depth + 1, resolves, rows);
            rows.push(NodeRow {
                depth,
                label: String::new(),
                kind: RowKind::LoopEnd,
                path: prefix.clone(),
                req_ok: None,
            });
        } else {
            rows.push(NodeRow {
                depth,
                label: node.label(),
                kind: RowKind::Leaf,
                path: prefix.clone(),
                req_ok,
            });
        }
        prefix.pop();
    }
}

// ---------------------------------------------------------------------------
// AST navigation & mutation
// ---------------------------------------------------------------------------

/// Where a newly inserted node lands: the containing loop body (`parent`, empty
/// = top level) and the index within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InsertPos {
    pub(crate) parent: Vec<usize>,
    pub(crate) index: usize,
}

/// The insertion point implied by the selected row: after a leaf, as the first
/// child of a `FOR` header, after the loop when on its `END`, or at the very
/// front when on `Begin`. So put the cursor on a `FOR` header and insert to go
/// *inside* it; on its `END` to go *after* it.
pub(crate) fn insert_pos_after(rows: &[NodeRow], sel: usize) -> InsertPos {
    let Some(row) = rows.get(sel) else {
        return InsertPos {
            parent: Vec::new(),
            index: 0,
        };
    };
    match row.kind {
        RowKind::Begin => InsertPos {
            parent: Vec::new(),
            index: 0,
        },
        RowKind::LoopHead => InsertPos {
            parent: row.path.clone(),
            index: 0,
        },
        RowKind::Leaf | RowKind::LoopEnd => {
            let (last, rest) = row.path.split_last().unwrap_or((&0, &[]));
            InsertPos {
                parent: rest.to_vec(),
                index: last + 1,
            }
        }
    }
}

fn loop_body(node: &FlowNode) -> Option<&Vec<FlowNode>> {
    match node {
        FlowNode::ForEach { body, .. } | FlowNode::ForEnvs { body, .. } => Some(body),
        _ => None,
    }
}

/// The source directory of a `FOR … IN FILES/FOLDERS` node, or `None` for any
/// other node (only these two producers carry a browsable folder).
pub(crate) fn loop_producer_dir(node: &FlowNode) -> Option<&str> {
    match node {
        FlowNode::ForEach {
            producer: Producer::Files { dir, .. } | Producer::Folders { dir, .. },
            ..
        } => Some(dir),
        _ => None,
    }
}

/// Mutable counterpart to [`loop_producer_dir`].
pub(crate) fn loop_producer_dir_mut(node: &mut FlowNode) -> Option<&mut String> {
    match node {
        FlowNode::ForEach {
            producer: Producer::Files { dir, .. } | Producer::Folders { dir, .. },
            ..
        } => Some(dir),
        _ => None,
    }
}

/// Mutable reference to the body Vec addressed by `parent` (empty = top level).
fn body_at_mut<'a>(flow: &'a mut ReportFlow, parent: &[usize]) -> Option<&'a mut Vec<FlowNode>> {
    let mut body = &mut flow.nodes;
    for &i in parent {
        body = body.get_mut(i)?.body_mut()?;
    }
    Some(body)
}

pub(crate) fn node_at<'a>(flow: &'a ReportFlow, path: &[usize]) -> Option<&'a FlowNode> {
    let (last, rest) = path.split_last()?;
    let mut body = &flow.nodes;
    for &i in rest {
        body = loop_body(body.get(i)?)?;
    }
    body.get(*last)
}

pub(crate) fn node_at_mut<'a>(
    flow: &'a mut ReportFlow,
    path: &[usize],
) -> Option<&'a mut FlowNode> {
    let (last, rest) = path.split_last()?;
    let body = body_at_mut(flow, rest)?;
    body.get_mut(*last)
}

pub(crate) fn insert_node(flow: &mut ReportFlow, pos: &InsertPos, node: FlowNode) {
    if let Some(body) = body_at_mut(flow, &pos.parent) {
        let idx = pos.index.min(body.len());
        body.insert(idx, node);
    }
}

pub(crate) fn remove_node(flow: &mut ReportFlow, path: &[usize]) -> bool {
    let Some((last, rest)) = path.split_last() else {
        return false;
    };
    if let Some(body) = body_at_mut(flow, rest)
        && *last < body.len()
    {
        body.remove(*last);
        return true;
    }
    false
}

/// Swap the node at `path` with its previous (`up`) / next sibling in the same
/// body. Returns the moved node's new path, or `None` at a boundary.
pub(crate) fn move_node(flow: &mut ReportFlow, path: &[usize], up: bool) -> Option<Vec<usize>> {
    let (last, rest) = path.split_last()?;
    let body = body_at_mut(flow, rest)?;
    let target = if up {
        last.checked_sub(1)?
    } else if last + 1 < body.len() {
        last + 1
    } else {
        return None;
    };
    body.swap(*last, target);
    let mut new_path = rest.to_vec();
    new_path.push(target);
    Some(new_path)
}

/// Replace the node at `path` with `new_node`, carrying the old loop body over
/// when both are loops (so editing a `FOR` header keeps its children).
pub(crate) fn replace_node(flow: &mut ReportFlow, path: &[usize], mut new_node: FlowNode) -> bool {
    let Some(slot) = node_at_mut(flow, path) else {
        return false;
    };
    let old_body = slot.body_mut().map(std::mem::take);
    if let (Some(ob), Some(nb)) = (old_body, new_node.body_mut()) {
        *nb = ob;
    }
    *slot = new_node;
    true
}

/// Parse one edited statement line back into a node. A loop needs a following
/// `END` to re-parse, so both a bare and an `END`-terminated form are tried
/// (loop-first when the original node was a loop). Returns `None` if the text
/// doesn't yield exactly one statement.
pub(crate) fn parse_one_node(text: &str, prefer_loop: bool) -> Option<FlowNode> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let bare = format!("{t}\n");
    let looped = format!("{t}\nEND\n");
    let attempts = if prefer_loop {
        [looped, bare]
    } else {
        [bare, looped]
    };
    for wrap in attempts {
        if let Ok(flow) = parse_flow(&wrap)
            && flow.nodes.len() == 1
        {
            return flow.nodes.into_iter().next();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The insert palette's node kinds
// ---------------------------------------------------------------------------

/// The kinds of node the insert palette offers, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NodeKind {
    Request,
    ReportRequest,
    ReportVar,
    Assign,
    ForFiles,
    ForFolders,
    ForEnvs,
    List,
}

impl NodeKind {
    pub(crate) const ALL: [NodeKind; 8] = [
        NodeKind::Request,
        NodeKind::ReportRequest,
        NodeKind::ReportVar,
        NodeKind::Assign,
        NodeKind::ForFiles,
        NodeKind::ForFolders,
        NodeKind::ForEnvs,
        NodeKind::List,
    ];

    /// The palette label for this kind.
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            NodeKind::Request => s.node_kind_request,
            NodeKind::ReportRequest => s.node_kind_report_request,
            NodeKind::ReportVar => s.node_kind_report_var,
            NodeKind::Assign => s.node_kind_assign,
            NodeKind::ForFiles => s.node_kind_for_files,
            NodeKind::ForFolders => s.node_kind_for_folders,
            NodeKind::ForEnvs => s.node_kind_for_envs,
            NodeKind::List => s.node_kind_list,
        }
    }

    /// Whether creating this kind needs a request name from the picker.
    pub(crate) fn needs_request(self) -> bool {
        matches!(self, NodeKind::Request | NodeKind::ReportRequest)
    }

    /// A template node with placeholder fields (for the non-request kinds); the
    /// user then fills the fields in via the "edit as line" prompt.
    pub(crate) fn template(self) -> Option<FlowNode> {
        Some(match self {
            NodeKind::Request | NodeKind::ReportRequest => return None,
            NodeKind::ReportVar => FlowNode::Report(ReportStmt::Vars(vec!["VAR".into()])),
            NodeKind::Assign => FlowNode::Assign {
                key: "NAME".into(),
                value: String::new(),
            },
            NodeKind::ForFiles => FlowNode::ForEach {
                pattern: Pattern::single("FILE"),
                producer: Producer::Files {
                    dir: String::new(),
                    glob: None,
                },
                body: Vec::new(),
                parallel: None,
            },
            NodeKind::ForFolders => FlowNode::ForEach {
                pattern: Pattern::single("FOLDER"),
                producer: Producer::Folders {
                    dir: String::new(),
                    roles: Vec::new(),
                },
                body: Vec::new(),
                parallel: None,
            },
            NodeKind::ForEnvs => FlowNode::ForEnvs {
                var: "TARGET".into(),
                // A placeholder BASELINE/COMPARISON pair — the comparison is the
                // whole point of an `ENVS` loop, so the inserted template shows
                // the shape to fill in. An empty clause would serialize to a
                // bare `FOR TARGET IN ENVS ` which doesn't re-parse (it would
                // kick the user out of the node editor). The names are just
                // placeholders the user replaces with real loaded environments.
                clause: EnvClause::Roles {
                    baseline: vec![RoleRef::Env("baseline".into())],
                    comparisons: vec![RoleRef::Env("candidate".into())],
                    baseline_show: Vec::new(),
                },
                body: Vec::new(),
                parallel: None,
            },
            NodeKind::List => FlowNode::ListDecl {
                name: "ITEMS".into(),
                producer: Producer::List(Vec::new()),
            },
        })
    }
}

/// A `REQUEST <name>` / `REPORT REQUEST <name>` node with the chosen name.
pub(crate) fn request_node(name: &str, report: bool) -> FlowNode {
    if report {
        FlowNode::Report(ReportStmt::Request {
            name: name.to_string(),
            alias: None,
            response_fmt: None,
            show: Vec::new(),
            hide: Vec::new(),
            with: Vec::new(),
        })
    } else {
        FlowNode::Request {
            name: name.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Compositional modifiers (the drag-on chips: REPORT / PARALLEL / WITH / AS)
// ---------------------------------------------------------------------------

/// A modifier that a compositional block editor drags *onto* an existing node
/// to attach it. Unlike a [`NodeKind`] (a whole new statement), a modifier
/// transforms the node it lands on: `REPORT` wraps a `REQUEST`/marks a report,
/// `PARALLEL` marks a loop concurrent, `WITH` adds an ad-hoc field to a report
/// request, and `AS` names/aliases a report column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Modifier {
    Report,
    Parallel,
    With,
    As,
}

impl Modifier {
    pub(crate) const ALL: [Modifier; 4] = [
        Modifier::Report,
        Modifier::Parallel,
        Modifier::With,
        Modifier::As,
    ];

    /// The palette label for this modifier.
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            Modifier::Report => s.node_mod_report,
            Modifier::Parallel => s.node_mod_parallel,
            Modifier::With => s.node_mod_with,
            Modifier::As => s.node_mod_as,
        }
    }

    /// Whether this modifier can be attached to `node` (drives both the drop
    /// highlight and whether a release does anything). A modifier that is
    /// already present, or nonsensical for the node, is not applicable.
    pub(crate) fn applies_to(self, node: &FlowNode) -> bool {
        match self {
            // REPORT wraps a plain (send-only) request into a reported one. A
            // variable is reported the moment it exists (there is no bare
            // variable node), so REPORT is only *attachable* to a `REQUEST`.
            Modifier::Report => matches!(node, FlowNode::Request { .. }),
            // PARALLEL marks a not-yet-parallel loop concurrent.
            Modifier::Parallel => matches!(
                node,
                FlowNode::ForEach { parallel: None, .. } | FlowNode::ForEnvs { parallel: None, .. }
            ),
            // WITH adds an ad-hoc field to a report request.
            Modifier::With => matches!(node, FlowNode::Report(ReportStmt::Request { .. })),
            // AS names a report column: an as-less report request, or a
            // single-variable `REPORT <var>` (which becomes `REPORT <var> AS …`).
            Modifier::As => match node {
                FlowNode::Report(ReportStmt::Request { alias, .. }) => alias.is_none(),
                FlowNode::Report(ReportStmt::Vars(vars)) => vars.len() == 1,
                _ => false,
            },
        }
    }
}

/// Which attached modifier a detach (the chip's `×`) targets. `With` carries the
/// index of the field to drop, since a report request can hold several.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DetachWhich {
    Report,
    Parallel,
    As,
    With(usize),
    /// The `RESPONSE RAW/PRETTY` override on a report request.
    Response,
    /// The `SHOW(…)` field selector on a report request.
    Show,
    /// The `HIDE(…)` field selector on a report request.
    Hide,
}

/// Attach `m` to the node at `path` (see [`Modifier::applies_to`]). No-op when
/// the modifier does not apply. Returns whether anything changed.
pub(crate) fn attach_modifier(flow: &mut ReportFlow, path: &[usize], m: Modifier) -> bool {
    let Some(node) = node_at_mut(flow, path) else {
        return false;
    };
    if !m.applies_to(node) {
        return false;
    }
    match m {
        Modifier::Report => {
            if let FlowNode::Request { name } = node {
                let name = std::mem::take(name);
                *node = FlowNode::Report(ReportStmt::Request {
                    name,
                    alias: None,
                    response_fmt: None,
                    show: Vec::new(),
                    hide: Vec::new(),
                    with: Vec::new(),
                });
            }
        }
        Modifier::Parallel => match node {
            FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. } => {
                *parallel = Some(ParallelSpec::default());
            }
            _ => {}
        },
        Modifier::With => {
            if let FlowNode::Report(ReportStmt::Request { with, .. }) = node {
                with.push(WithItem::Field {
                    name: "field".into(),
                    query: "HttpStatus".into(),
                    stats: Vec::new(),
                });
            }
        }
        Modifier::As => match node {
            FlowNode::Report(ReportStmt::Request { alias, .. }) => {
                *alias = Some("alias".into());
            }
            FlowNode::Report(ReportStmt::Vars(vars)) if vars.len() == 1 => {
                let var = vars.remove(0);
                *node = FlowNode::Report(ReportStmt::VarAs {
                    var,
                    name: "name".into(),
                    stats: Vec::new(),
                });
            }
            _ => {}
        },
    }
    true
}

/// Rename the request the node at `path` references, in place — preserving all
/// of a report request's modifiers (`AS` alias, `WITH` fields, `RESPONSE` /
/// `SHOW` / `HIDE`). Works for both a plain `REQUEST` and a `REPORT REQUEST`.
/// Returns whether anything changed.
pub(crate) fn set_request_name(flow: &mut ReportFlow, path: &[usize], name: &str) -> bool {
    match node_at_mut(flow, path) {
        Some(FlowNode::Request { name: n })
        | Some(FlowNode::Report(ReportStmt::Request { name: n, .. })) => {
            *n = name.to_string();
            true
        }
        _ => false,
    }
}
/// whole node should now be *removed* — detaching `REPORT` from a reported
/// variable/computed column leaves no valid statement behind (there is no bare
/// variable node), so the caller drops the row entirely.
pub(crate) fn detach_modifier(flow: &mut ReportFlow, path: &[usize], which: DetachWhich) -> bool {
    let Some(node) = node_at_mut(flow, path) else {
        return false;
    };
    match which {
        DetachWhich::Report => match node {
            // A reported request keeps sending: downgrade to a plain REQUEST.
            FlowNode::Report(ReportStmt::Request { name, .. }) => {
                let name = std::mem::take(name);
                *node = FlowNode::Request { name };
                false
            }
            // A reported variable/computed column has nothing left without
            // REPORT — signal the caller to remove the row.
            FlowNode::Report(_) => true,
            _ => false,
        },
        DetachWhich::Parallel => {
            match node {
                FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. } => {
                    *parallel = None;
                }
                _ => {}
            }
            false
        }
        DetachWhich::As => {
            match node {
                FlowNode::Report(ReportStmt::Request { alias, .. }) => *alias = None,
                FlowNode::Report(ReportStmt::VarAs { var, .. }) => {
                    let var = std::mem::take(var);
                    *node = FlowNode::Report(ReportStmt::Vars(vec![var]));
                }
                _ => {}
            }
            false
        }
        DetachWhich::With(i) => {
            if let FlowNode::Report(ReportStmt::Request { with, .. }) = node
                && i < with.len()
            {
                with.remove(i);
            }
            false
        }
        DetachWhich::Response => {
            if let FlowNode::Report(ReportStmt::Request { response_fmt, .. }) = node {
                *response_fmt = None;
            }
            false
        }
        DetachWhich::Show => {
            if let FlowNode::Report(ReportStmt::Request { show, .. }) = node {
                show.clear();
            }
            false
        }
        DetachWhich::Hide => {
            if let FlowNode::Report(ReportStmt::Request { hide, .. }) = node {
                hide.clear();
            }
            false
        }
    }
}

/// Take (remove and return) the node at `path`, or `None` when the path does
/// not address a node. Used to relocate an existing node for drag-to-reorder.
pub(crate) fn take_node(flow: &mut ReportFlow, path: &[usize]) -> Option<FlowNode> {
    let (last, rest) = path.split_last()?;
    let body = body_at_mut(flow, rest)?;
    if *last < body.len() {
        Some(body.remove(*last))
    } else {
        None
    }
}

/// Move the existing node at `from` to the insert position `pos`, returning the
/// moved node's new path. Used when an in-report block is dragged onto a drop
/// strip to reorder it. A no-op (returns `None`) when `from` would move into its
/// own subtree (a loop cannot contain itself), keeping the tree well-formed.
///
/// Because removing the source shifts later siblings down by one, the
/// destination index is adjusted when both share a parent and the target sits
/// after the removed slot.
pub(crate) fn move_node_to(
    flow: &mut ReportFlow,
    from: &[usize],
    pos: &InsertPos,
) -> Option<Vec<usize>> {
    // Refuse to drop a loop inside itself (its own body / a descendant body):
    // `pos.parent` starting with `from` would orphan the subtree.
    if pos.parent.len() >= from.len() && pos.parent[..from.len()] == *from {
        return None;
    }
    let node = take_node(flow, from)?;
    let (from_last, from_parent) = from.split_last()?;
    // Removing the source shifts the later children of *its* body down by one.
    // A destination that traverses that same body at a slot after the removed
    // one must have that single index decremented — whether the slot is the
    // insertion index itself (same body) or a component of the parent path (the
    // destination nests through the body below the removed node).
    let d = from_parent.len();
    let mut parent = pos.parent.clone();
    let mut index = pos.index;
    if parent.len() > d && parent[..d] == *from_parent {
        if parent[d] > *from_last {
            parent[d] -= 1;
        }
    } else if parent == *from_parent && *from_last < index {
        index -= 1;
    }
    let dest = InsertPos { parent, index };
    insert_node(flow, &dest, node);
    let mut new_path = dest.parent;
    new_path.push(dest.index);
    Some(new_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::flow::FlowNode;

    fn flow(src: &str) -> ReportFlow {
        parse_flow(src).expect("test flow must parse")
    }

    fn always_ok(_: &str) -> bool {
        true
    }

    #[test]
    fn flatten_marks_begin_body_and_loop_end() {
        let f = flow("REQUEST A\nFOR X IN FILES \"/d\"\n    REQUEST B\nEND\n");
        let rows = flatten(&f, &always_ok);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::Begin,
                RowKind::Leaf,
                RowKind::LoopHead,
                RowKind::Leaf,
                RowKind::LoopEnd,
            ]
        );
        // The loop head and its END share the same path; the body is deeper.
        assert_eq!(rows[2].path, rows[4].path);
        assert_eq!(rows[3].path, vec![1, 0]);
    }

    #[test]
    fn insert_pos_after_targets_the_right_body() {
        let f = flow("REQUEST A\nFOR X IN FILES \"/d\"\n    REQUEST B\nEND\n");
        let rows = flatten(&f, &always_ok);
        // On Begin (row 0): front of the top level.
        let p = insert_pos_after(&rows, 0);
        assert_eq!(p.parent, Vec::<usize>::new());
        assert_eq!(p.index, 0);
        // On the leaf REQUEST A (row 1): after it, same (top) level.
        let p = insert_pos_after(&rows, 1);
        assert_eq!(p.parent, Vec::<usize>::new());
        assert_eq!(p.index, 1);
        // On the loop head (row 2): inside the loop, at its front.
        let p = insert_pos_after(&rows, 2);
        assert_eq!(p.parent, vec![1]);
        assert_eq!(p.index, 0);
        // On END (row 4): after the whole loop, at the top level.
        let p = insert_pos_after(&rows, 4);
        assert_eq!(p.parent, Vec::<usize>::new());
        assert_eq!(p.index, 2);
    }

    #[test]
    fn insert_and_remove_round_trip() {
        let mut f = flow("REQUEST A\n");
        let pos = InsertPos {
            parent: Vec::new(),
            index: 1,
        };
        insert_node(&mut f, &pos, request_node("B", false));
        assert_eq!(
            f.nodes.iter().map(|n| n.request_name()).collect::<Vec<_>>(),
            vec![Some("A"), Some("B")]
        );
        assert!(remove_node(&mut f, &[0]));
        assert_eq!(
            f.nodes.iter().map(|n| n.request_name()).collect::<Vec<_>>(),
            vec![Some("B")]
        );
    }

    #[test]
    fn move_node_swaps_siblings_and_reports_boundaries() {
        let mut f = flow("REQUEST A\nREQUEST B\n");
        // Move B (index 1) up -> becomes index 0.
        let np = move_node(&mut f, &[1], true).expect("can move up");
        assert_eq!(np, vec![0]);
        assert_eq!(
            f.nodes.iter().map(|n| n.request_name()).collect::<Vec<_>>(),
            vec![Some("B"), Some("A")]
        );
        // The first node can't move up any further.
        assert!(move_node(&mut f, &[0], true).is_none());
    }

    #[test]
    fn replace_node_keeps_a_loops_body() {
        let mut f = flow("FOR X IN FILES \"/a\"\n    REQUEST Inner\nEND\n");
        // Re-parse an edited FOR header (a different dir) and swap it in.
        let edited = parse_one_node("FOR X IN FILES \"/b\"", true).expect("loop parses");
        assert!(replace_node(&mut f, &[0], edited));
        // The body survived the header replacement.
        let body = match &f.nodes[0] {
            FlowNode::ForEach { body, .. } => body,
            other => panic!("expected a loop, got {other:?}"),
        };
        assert_eq!(body.len(), 1);
        assert_eq!(body[0].request_name(), Some("Inner"));
        // And the new dir is reflected in the serialized text.
        assert!(f.to_text().contains("\"/b\""));
    }

    #[test]
    fn parse_one_node_needs_exactly_one_statement() {
        assert!(parse_one_node("REQUEST A", false).is_some());
        assert!(parse_one_node("FOR X IN FILES \"/d\"", true).is_some());
        // Two statements is not a single node.
        assert!(parse_one_node("REQUEST A\nREQUEST B", false).is_none());
        // A bare FOR with no END never closes.
        assert!(parse_one_node("FOR", false).is_none());
        assert!(parse_one_node("   ", false).is_none());
    }

    /// The node editor's raw line prompt routes through `parse_one_node`, so a
    /// `REPORT VAR AS <pretty name>` line becomes a renamed-variable node.
    #[test]
    fn parse_one_node_accepts_report_var_as() {
        use crate::report::flow::ReportStmt;
        match parse_one_node("REPORT FILE AS \"Pretty name\"", false) {
            Some(FlowNode::Report(ReportStmt::VarAs { var, name, .. })) => {
                assert_eq!(var, "FILE");
                assert_eq!(name, "Pretty name");
            }
            other => panic!("expected a VarAs node, got {other:?}"),
        }
    }

    // ── Compositional modifiers ────────────────────────────────────────────

    #[test]
    fn report_modifier_wraps_and_unwraps_a_request() {
        let mut f = flow("REQUEST login\n");
        assert!(Modifier::Report.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(attach_modifier(&mut f, &[0], Modifier::Report));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::Report(ReportStmt::Request { .. }))
        ));
        // REPORT no longer applies (already reported); detaching restores the send.
        assert!(!Modifier::Report.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::Report));
        assert!(matches!(node_at(&f, &[0]), Some(FlowNode::Request { .. })));
    }

    #[test]
    fn detaching_report_from_a_variable_asks_to_remove_the_row() {
        let mut f = flow("REPORT userId\n");
        // A reported variable has nothing valid left without REPORT.
        assert!(detach_modifier(&mut f, &[0], DetachWhich::Report));
    }

    #[test]
    fn parallel_modifier_toggles_a_loop() {
        let mut f = flow("FOR X IN FILES \"/d\"\n    REQUEST A\nEND\n");
        assert!(Modifier::Parallel.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(attach_modifier(&mut f, &[0], Modifier::Parallel));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::ForEach {
                parallel: Some(_),
                ..
            })
        ));
        // Body is preserved and PARALLEL no longer applies.
        assert!(!Modifier::Parallel.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::Parallel));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::ForEach { parallel: None, .. })
        ));
    }

    #[test]
    fn with_modifier_adds_and_removes_a_report_request_field() {
        let mut f = flow("REPORT REQUEST analyze\n");
        assert!(Modifier::With.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(attach_modifier(&mut f, &[0], Modifier::With));
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request { with, .. })) => {
                assert_eq!(with.len(), 1);
            }
            other => panic!("expected a report request with a field, got {other:?}"),
        }
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::With(0)));
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request { with, .. })) => assert!(with.is_empty()),
            other => panic!("expected an empty WITH, got {other:?}"),
        }
    }

    #[test]
    fn as_modifier_names_a_request_alias_and_a_variable_column() {
        // On a report request → sets the alias.
        let mut f = flow("REPORT REQUEST analyze\n");
        assert!(attach_modifier(&mut f, &[0], Modifier::As));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::Report(ReportStmt::Request { alias: Some(_), .. }))
        ));
        // AS no longer applies once aliased.
        assert!(!Modifier::As.applies_to(node_at(&f, &[0]).unwrap()));

        // On a single-variable REPORT → becomes a VarAs column.
        let mut g = flow("REPORT userId\n");
        assert!(Modifier::As.applies_to(node_at(&g, &[0]).unwrap()));
        assert!(attach_modifier(&mut g, &[0], Modifier::As));
        assert!(matches!(
            node_at(&g, &[0]),
            Some(FlowNode::Report(ReportStmt::VarAs { .. }))
        ));
        // Detaching AS returns it to a bare REPORT <var>.
        assert!(!detach_modifier(&mut g, &[0], DetachWhich::As));
        assert!(matches!(
            node_at(&g, &[0]),
            Some(FlowNode::Report(ReportStmt::Vars(_)))
        ));
    }

    #[test]
    fn modifiers_do_not_apply_where_they_make_no_sense() {
        let f = flow("k = v\n");
        let n = node_at(&f, &[0]).unwrap();
        assert!(!Modifier::Report.applies_to(n));
        assert!(!Modifier::Parallel.applies_to(n));
        assert!(!Modifier::With.applies_to(n));
        assert!(!Modifier::As.applies_to(n));
    }

    #[test]
    fn renaming_a_report_request_preserves_its_modifiers() {
        let mut f = flow("REPORT REQUEST analyze AS proc WITH\n    latency: Time\nEND\n");
        assert!(set_request_name(&mut f, &[0], "verify"));
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request {
                name, alias, with, ..
            })) => {
                assert_eq!(name, "verify");
                assert_eq!(alias.as_deref(), Some("proc"));
                assert_eq!(with.len(), 1);
            }
            other => panic!("expected the report request kept its modifiers, got {other:?}"),
        }
    }

    #[test]
    fn detaching_response_show_hide_clears_only_that_clause() {
        let mut f =
            flow("REPORT REQUEST analyze RESPONSE RAW SHOW(Time, HttpStatus) HIDE(Response)\n");
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::Response));
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::Show));
        assert!(!detach_modifier(&mut f, &[0], DetachWhich::Hide));
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request {
                name,
                response_fmt,
                show,
                hide,
                ..
            })) => {
                assert_eq!(name, "analyze");
                assert!(response_fmt.is_none());
                assert!(show.is_empty());
                assert!(hide.is_empty());
            }
            other => panic!("expected the request kept only its name, got {other:?}"),
        }
    }

    #[test]
    fn move_node_to_reorders_within_a_body_and_adjusts_the_index() {
        // A, B, C at top level. Move A (index 0) to index 2 (after B, before C).
        let mut f = flow("REQUEST A\nREQUEST B\nREQUEST C\n");
        let pos = InsertPos {
            parent: Vec::new(),
            index: 2,
        };
        let new = move_node_to(&mut f, &[0], &pos).expect("move should succeed");
        // The removal of index 0 shifts the target down by one → lands at 1.
        assert_eq!(new, vec![1]);
        let names: Vec<String> = f
            .nodes
            .iter()
            .map(|n| match n {
                FlowNode::Request { name } => name.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, vec!["B", "A", "C"]);
    }

    #[test]
    fn move_node_to_can_nest_into_a_loop_body() {
        let mut f = flow("REQUEST A\nFOR X IN FILES \"/d\"\n    REQUEST B\nEND\n");
        // Move A (index 0) into the loop body (path [1]) at index 0.
        let pos = InsertPos {
            parent: vec![1],
            index: 0,
        };
        let new = move_node_to(&mut f, &[0], &pos).expect("move should succeed");
        assert_eq!(new, vec![0, 0]);
        // Now the loop is the only top-level node, holding A then B.
        match &f.nodes[0] {
            FlowNode::ForEach { body, .. } => {
                assert_eq!(body.len(), 2);
                assert!(matches!(&body[0], FlowNode::Request { name } if name == "A"));
            }
            other => panic!("expected the loop, got {other:?}"),
        }
    }

    #[test]
    fn move_node_to_refuses_to_drop_a_loop_into_itself() {
        let mut f = flow("FOR X IN FILES \"/d\"\n    REQUEST B\nEND\n");
        // Try to move the loop (path [0]) into its own body (parent [0]).
        let pos = InsertPos {
            parent: vec![0],
            index: 0,
        };
        assert!(move_node_to(&mut f, &[0], &pos).is_none());
        // The tree is untouched.
        assert!(matches!(&f.nodes[0], FlowNode::ForEach { .. }));
    }
}
