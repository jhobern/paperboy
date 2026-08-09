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

// The modifier half of this API (attach / detach / carry / transfer) exists for
// the GUI's block editor — the terminal UI reaches its node editor through its
// own wizards and uses only the insert/move/replace half. It stays compiled in a
// terminal-only build so this module remains one shared core with one test
// suite, rather than splintering along a front-end boundary.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use crate::i18n::Strings;
use crate::report::flow::{
    Binder, EnvClause, FlowNode, HeaderLine, ParallelSpec, Pattern, Producer, ReportFlow,
    ReportStmt, ResponseFmt, RoleRef, WithItem,
};
use crate::report::model::StatKind;
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
    /// A `# …` comment line. A node like any other — it can be selected, moved
    /// and deleted — but it is drawn dimmed, because it isn't a statement.
    Comment,
    /// One field of an expanded `REPORT REQUEST … WITH … END` block, at this
    /// index into the request's `with` list. `path` addresses the *request*, not
    /// the field, so anything acting on a `WITH` row must branch on this kind
    /// rather than treating the path as a node to delete or move.
    WithField(usize),
    /// The "add a field" affordance at the end of an expanded `WITH` block.
    WithAdd,
    /// The synthetic `END` closing an expanded `WITH` block.
    WithEnd,
}

impl RowKind {
    /// Whether this row belongs to an expanded `WITH` block rather than being a
    /// flow node in its own right.
    pub(crate) fn is_with(self) -> bool {
        matches!(
            self,
            RowKind::WithField(_) | RowKind::WithAdd | RowKind::WithEnd
        )
    }
}

/// Flatten a flow into the display rows, tagging request rows with whether they
/// resolve (via `resolves`). Row 0 is the `Begin` root.
///
/// `WITH` blocks stay collapsed to a single `… WITH …` row — see
/// [`flatten_expanded`] for the form that opens them up.
pub(crate) fn flatten(flow: &ReportFlow, resolves: &impl Fn(&str) -> bool) -> Vec<NodeRow> {
    flatten_expanded(flow, resolves, false)
}

/// As [`flatten`], but with `expand_with` a `REPORT REQUEST … WITH` block is
/// opened out: the request row, one [`RowKind::WithField`] row per field, an
/// [`RowKind::WithAdd`] row, and a closing [`RowKind::WithEnd`].
///
/// It is opt-in because the two front-ends show `WITH` differently — the GUI
/// draws each field as a chip on the request's own row, so expanding would
/// double it up, while the TUI outline has no room for chips and needs the rows.
pub(crate) fn flatten_expanded(
    flow: &ReportFlow,
    resolves: &impl Fn(&str) -> bool,
    expand_with: bool,
) -> Vec<NodeRow> {
    let mut rows = vec![NodeRow {
        depth: 0,
        label: String::new(),
        kind: RowKind::Begin,
        path: Vec::new(),
        req_ok: None,
    }];
    let mut prefix = Vec::new();
    push_nodes(
        &flow.nodes,
        &mut prefix,
        1,
        resolves,
        expand_with,
        &mut rows,
    );
    rows
}

/// The `WITH` fields of a report-request node, or `None` for anything else.
pub(crate) fn node_with_items(node: &FlowNode) -> Option<&[WithItem]> {
    match node {
        FlowNode::Report(ReportStmt::Request { with, .. }) => Some(with),
        _ => None,
    }
}

/// The row label for one `WITH` item — the same text it is written with in
/// source, so the outline and the source view read alike.
pub(crate) fn with_item_label(item: &WithItem) -> String {
    crate::report::flow::with_item_text(item)
}

fn push_nodes(
    nodes: &[FlowNode],
    prefix: &mut Vec<usize>,
    depth: usize,
    resolves: &impl Fn(&str) -> bool,
    expand_with: bool,
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
            push_nodes(body, prefix, depth + 1, resolves, expand_with, rows);
            rows.push(NodeRow {
                depth,
                label: String::new(),
                kind: RowKind::LoopEnd,
                path: prefix.clone(),
                req_ok: None,
            });
        } else {
            let with = expand_with
                .then(|| node_with_items(node))
                .flatten()
                .filter(|w| !w.is_empty());
            rows.push(NodeRow {
                depth,
                // Expanded, the head loses its "…" placeholder: the fields it
                // stood for are the rows immediately below.
                label: match with {
                    Some(_) => format!("{} WITH", node.header_line()),
                    None => node.label(),
                },
                kind: match node {
                    FlowNode::Comment(_) => RowKind::Comment,
                    _ => RowKind::Leaf,
                },
                path: prefix.clone(),
                req_ok,
            });
            if let Some(with) = with {
                for (wi, item) in with.iter().enumerate() {
                    rows.push(NodeRow {
                        depth: depth + 1,
                        label: with_item_label(item),
                        kind: RowKind::WithField(wi),
                        path: prefix.clone(),
                        req_ok: None,
                    });
                }
                rows.push(NodeRow {
                    depth: depth + 1,
                    label: String::new(),
                    kind: RowKind::WithAdd,
                    path: prefix.clone(),
                    req_ok: None,
                });
                rows.push(NodeRow {
                    depth,
                    label: String::new(),
                    kind: RowKind::WithEnd,
                    path: prefix.clone(),
                    req_ok: None,
                });
            }
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
        // A `WITH` row isn't a flow node, but a flow insert asked for from one
        // still has to land *somewhere* sensible: after the request that owns
        // the block, which is where the whole `WITH … END` ends on screen.
        RowKind::Leaf
        | RowKind::Comment
        | RowKind::LoopEnd
        | RowKind::WithField(_)
        | RowKind::WithAdd
        | RowKind::WithEnd => {
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
    ReportComputed,
    Assign,
    ForFiles,
    ForFolders,
    ForEnvs,
    List,
}

impl NodeKind {
    pub(crate) const ALL: [NodeKind; 9] = [
        NodeKind::Request,
        NodeKind::ReportRequest,
        NodeKind::ReportVar,
        NodeKind::ReportComputed,
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
            NodeKind::ReportComputed => s.node_kind_report_computed,
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
            NodeKind::ReportComputed => FlowNode::Report(ReportStmt::Computed {
                // A placeholder computed column the user edits via its wizard.
                // The template must be a non-empty string and it must carry an
                // AS name, or `REPORT "…"` won't re-parse (kicking the user out
                // of the node editor).
                template: "value".into(),
                name: "column".into(),
                stats: Vec::new(),
                image: None,
            }),
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
                    glob: None,
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
    Response,
    Show,
    Hide,
    /// `STATISTICS(…)` — summary rows for a named report column.
    Statistics,
}

impl Modifier {
    pub(crate) const ALL: [Modifier; 8] = [
        Modifier::Report,
        Modifier::Parallel,
        Modifier::With,
        Modifier::As,
        Modifier::Response,
        Modifier::Show,
        Modifier::Hide,
        Modifier::Statistics,
    ];

    /// The palette label for this modifier.
    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            Modifier::Report => s.node_mod_report,
            Modifier::Parallel => s.node_mod_parallel,
            Modifier::With => s.node_mod_with,
            Modifier::As => s.node_mod_as,
            Modifier::Response => s.node_mod_response,
            Modifier::Show => s.node_mod_show,
            Modifier::Hide => s.node_mod_hide,
            Modifier::Statistics => s.node_mod_statistics,
        }
    }

    /// Whether this modifier can be attached to `node` (drives both the drop
    /// highlight and whether a release does anything). A modifier that is
    /// already present, or nonsensical for the node, is not applicable.
    pub(crate) fn applies_to(self, node: &FlowNode) -> bool {
        match self {
            // REPORT wraps a plain (send-only) request into a reported one, or
            // reports the variable a `SET` assignment defines (by inserting a
            // sibling `REPORT (VAR)` after it — see `report_assignment`).
            Modifier::Report => {
                matches!(node, FlowNode::Request { .. } | FlowNode::Assign { .. })
            }
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
            // RESPONSE / SHOW / HIDE all decorate a report request, and only
            // when it doesn't already carry that clause (so the drop reads as
            // "add it" and never silently overwrites an existing one).
            Modifier::Response => matches!(
                node,
                FlowNode::Report(ReportStmt::Request {
                    response_fmt: None,
                    ..
                })
            ),
            Modifier::Show => {
                matches!(node, FlowNode::Report(ReportStmt::Request { show, .. }) if show.is_empty())
            }
            Modifier::Hide => {
                matches!(node, FlowNode::Report(ReportStmt::Request { hide, .. }) if hide.is_empty())
            }
            // STATISTICS summarises a *named* report column, so it needs an
            // already-named one: `REPORT <var> AS <name>` or a computed column.
            // A bare `REPORT (A, B)` has no single column to summarise (attach
            // AS first), and a request's own columns are named by its WITH
            // fields, which carry their own STATISTICS.
            Modifier::Statistics => match node {
                FlowNode::Report(ReportStmt::VarAs { stats, .. })
                | FlowNode::Report(ReportStmt::Computed { stats, .. }) => stats.is_empty(),
                _ => false,
            },
        }
    }

    /// Why this modifier refuses to attach to `node`, or `None` when it does
    /// attach. A rejected drop used to be silent — the chip simply sprang back
    /// with no hint that the *block*, not the aim, was the problem. The two
    /// answers a user needs are "wrong kind of block" and "it's already there",
    /// so the reasons split along that line rather than restating `applies_to`.
    pub(crate) fn reject_reason(self, node: &FlowNode, s: &Strings) -> Option<&'static str> {
        if self.applies_to(node) {
            return None;
        }
        // A reported request is the target of most modifiers; when the block is
        // one, the only remaining reason is that the clause is already present.
        let reported = matches!(node, FlowNode::Report(ReportStmt::Request { .. }));
        Some(match self {
            // Any `REPORT …` statement — a reported request, a reported
            // variable, a computed column — already *is* the thing REPORT
            // adds, so the honest answer is "already there" rather than a
            // lecture about where REPORT goes.
            Modifier::Report => {
                if matches!(node, FlowNode::Report(_)) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_report
                }
            }
            Modifier::Parallel => {
                if matches!(node, FlowNode::ForEach { .. } | FlowNode::ForEnvs { .. }) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_parallel
                }
            }
            Modifier::With => s.mod_reject_with,
            Modifier::As => {
                if reported || matches!(node, FlowNode::Report(ReportStmt::Vars(v)) if v.len() == 1)
                {
                    s.mod_reject_present
                } else {
                    s.mod_reject_as
                }
            }
            Modifier::Response | Modifier::Show | Modifier::Hide => {
                if reported {
                    s.mod_reject_present
                } else {
                    s.mod_reject_request_only
                }
            }
            Modifier::Statistics => {
                if matches!(
                    node,
                    FlowNode::Report(ReportStmt::VarAs { .. })
                        | FlowNode::Report(ReportStmt::Computed { .. })
                ) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_statistics
                }
            }
        })
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
    /// The `SHOW(…)` clause hanging off an ENVS loop's `BASELINE`.
    BaselineShow,
    /// One `BASELINE`/`COMPARISON` role of an ENVS compare loop.
    Role {
        baseline: bool,
        index: usize,
    },
    /// The whole `WITH … END` block of a report request (its individual fields
    /// detach one at a time as [`DetachWhich::With`]).
    WithBlock,
    /// The `STATISTICS(…)` clause of a named report column.
    Statistics,
}

/// Every variable name in scope at `path` — the candidates a `REPORT <var>`
/// column can name.
///
/// Walks the flow down `path`, collecting what is bound *before* the node at
/// each level: assignments and the captures of requests already sent, plus the
/// binders of every enclosing loop (its pattern, and a `FOLDERS` loop's role
/// names). `entries` is the bound collection, used to resolve each request's
/// `[Captures]`; pass an empty slice when the collection is unknown and the
/// list simply won't include captures.
///
/// This is deliberately the *statically knowable* set: a `TUPLES FROM` or
/// `ZIP` loop can bind names that only exist at run time, and an environment
/// contributes its own keys, so the list is a helpful shortlist rather than an
/// exhaustive one. Both front-ends offer it alongside a free-text row for
/// exactly that reason.
pub(crate) fn vars_in_scope(
    flow: &ReportFlow,
    path: &[usize],
    entries: &[crate::hurl::HurlEntry],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |name: &str, out: &mut Vec<String>| {
        if !name.trim().is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    };
    let mut nodes: &[FlowNode] = &flow.nodes;
    for index in path {
        for node in nodes.iter().take(*index) {
            match node {
                FlowNode::Assign { key, .. } => push(key, &mut out),
                FlowNode::Request { name } | FlowNode::Report(ReportStmt::Request { name, .. }) => {
                    if let Some(entry) = crate::report::run::resolve_title(entries, name) {
                        for (cap, _) in &entry.captures {
                            push(cap, &mut out);
                        }
                    }
                }
                _ => {}
            }
        }
        // Stepping *into* the loop at `index` brings its binders into scope for
        // everything below, including the node we're heading towards.
        let Some(parent) = nodes.get(*index) else {
            break;
        };
        match parent {
            FlowNode::ForEach {
                pattern,
                producer,
                body,
                ..
            } => {
                for name in pattern.named() {
                    push(name, &mut out);
                }
                if let Producer::Folders { roles, .. } = producer {
                    for role in roles {
                        push(&role.name, &mut out);
                    }
                }
                nodes = body;
            }
            FlowNode::ForEnvs { var, body, .. } => {
                push(var, &mut out);
                nodes = body;
            }
            // The path claims to step into a loop but the node isn't one, so
            // there is nothing further to walk.
            _ => break,
        }
    }
    out
}

/// The fields a `BASELINE(…) SHOW(…)` can name, ticked where the clause already
/// names them.
///
/// A `SHOW` on a baseline selects from what the loop's *body* reports, so the
/// candidates are gathered by walking the body for reported requests and asking
/// the bound collection what each one emits — the same canonical order the
/// request form uses (intrinsics first, then the request's own `[Reports]`
/// fields). Anything already named by the clause is appended even if no request
/// claims it, so opening and applying the form can never silently drop a field
/// the user wrote by hand.
pub(crate) fn baseline_show_choices(
    entries: &[crate::hurl::HurlEntry],
    body: &[FlowNode],
    selected: &[String],
) -> Vec<(String, bool)> {
    let mut names: Vec<String> = Vec::new();
    let push = |n: &str, names: &mut Vec<String>| {
        if !n.trim().is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for f in crate::report::run::INTRINSIC_FIELDS {
        push(f, &mut names);
    }
    for req in reported_requests(body) {
        if let Some(entry) = crate::report::run::resolve_title(entries, &req) {
            for (f, _) in &entry.reports {
                push(f, &mut names);
            }
        }
    }
    for f in selected {
        push(f, &mut names);
    }
    names
        .iter()
        .map(|n| (n.clone(), selected.iter().any(|sel| sel == n)))
        .collect()
}

/// Every request name reported anywhere beneath `body`, nested loops included.
pub(crate) fn reported_requests(body: &[FlowNode]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(nodes: &[FlowNode], out: &mut Vec<String>) {
        for n in nodes {
            match n {
                // A bare `REQUEST x` sends but emits nothing, so it has no
                // fields to offer; only `REPORT REQUEST x` does.
                FlowNode::Report(ReportStmt::Request { name, .. }) => out.push(name.clone()),
                FlowNode::ForEnvs { body, .. } | FlowNode::ForEach { body, .. } => walk(body, out),
                _ => {}
            }
        }
    }
    walk(body, &mut out);
    out
}

/// Attach `m` to the node at `path` (see [`Modifier::applies_to`]). No-op when
/// the modifier does not apply. Returns whether anything changed.
pub(crate) fn attach_modifier(flow: &mut ReportFlow, path: &[usize], m: Modifier) -> bool {
    match node_at_mut(flow, path) {
        Some(node) => attach_to_node(node, m),
        None => false,
    }
}

/// The body of [`attach_modifier`], on an already-resolved node. Split out so a
/// caller can *rehearse* a drop on a throwaway clone — which is how the block
/// editor previews where a dragged modifier will land without the preview ever
/// being able to disagree with the real thing.
pub(crate) fn attach_to_node(node: &mut FlowNode, m: Modifier) -> bool {
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
                    image: None,
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
                    image: None,
                });
            }
            _ => {}
        },
        // RESPONSE / SHOW / HIDE seed a sensible default (PRETTY, and the first
        // intrinsic field) that the user then refines in the request wizard.
        Modifier::Response => {
            if let FlowNode::Report(ReportStmt::Request { response_fmt, .. }) = node {
                *response_fmt = Some(ResponseFmt::Pretty);
            }
        }
        Modifier::Show => {
            if let FlowNode::Report(ReportStmt::Request { show, .. }) = node
                && show.is_empty()
            {
                *show = vec!["HttpStatus".into()];
            }
        }
        Modifier::Hide => {
            if let FlowNode::Report(ReportStmt::Request { hide, .. }) = node
                && hide.is_empty()
            {
                *hide = vec!["HttpStatus".into()];
            }
        }
        // COUNT is the one statistic that means something for every column
        // (text included), so it is the safe seed; the wizard refines it.
        Modifier::Statistics => match node {
            FlowNode::Report(ReportStmt::VarAs { stats, .. })
            | FlowNode::Report(ReportStmt::Computed { stats, .. }) => {
                *stats = vec![StatKind::Count];
            }
            _ => {}
        },
    }
    true
}

/// Attach `STATISTICS(COUNT)` to the `WITH` field at `index` of the report
/// request at `path`, returning whether anything changed.
///
/// A `WITH` field is a report column in its own right — it has a name, and the
/// grammar lets it carry its own `STATISTICS(…)` — but it is not a `FlowNode`,
/// so [`attach_modifier`] (which addresses nodes by path) can't reach it. That
/// left the block editor able to *show* a field's `STATISTICS` while giving no
/// way to add one: the clause bounced off the `WITH` row, and dropping it on the
/// request line above attaches to nothing, because a request's columns are named
/// by its fields rather than by the request.
///
/// `COUNT` is the seed for the same reason it is in [`attach_to_node`]: it is
/// the one statistic that means something for a text column as well as a numeric
/// one. The field wizard refines it.
pub(crate) fn attach_with_stats(flow: &mut ReportFlow, path: &[usize], index: usize) -> bool {
    let Some(FlowNode::Report(ReportStmt::Request { with, .. })) = node_at_mut(flow, path) else {
        return false;
    };
    match with.get_mut(index) {
        Some(WithItem::Field { stats, .. }) if stats.is_empty() => {
            *stats = vec![StatKind::Count];
            true
        }
        _ => false,
    }
}

/// Whether [`attach_with_stats`] would do anything — i.e. whether the `WITH`
/// item at `index` is a named field that hasn't already got a `STATISTICS`
/// clause. Drives the drop highlight, so the preview and the drop agree.
pub(crate) fn with_stats_applies(with: &[WithItem], index: usize) -> bool {
    matches!(with.get(index), Some(WithItem::Field { stats, .. }) if stats.is_empty())
}

/// Report the variable a `SET` assignment at `path` defines: insert a
/// `REPORT (KEY)` statement immediately after the assignment (which itself
/// stays, since it is what actually sets the variable), returning the new
/// statement's path. A no-op (`None`) when `path` is not an `Assign`. This is
/// what dropping the `REPORT` modifier onto a `VARIABLE` block does — unlike a
/// request (transformed in place), an assignment needs a *separate* report
/// line.
pub(crate) fn report_assignment(flow: &mut ReportFlow, path: &[usize]) -> Option<Vec<usize>> {
    let key = match node_at(flow, path)? {
        FlowNode::Assign { key, .. } => key.clone(),
        _ => return None,
    };
    let (last, rest) = path.split_last()?;
    let mut existing = rest.to_vec();
    existing.push(last + 1);
    // Idempotent: if a `REPORT (KEY)` line already immediately follows the
    // assignment, don't stack another duplicate column — just select it.
    if let Some(FlowNode::Report(ReportStmt::Vars(vars))) = node_at(flow, &existing)
        && vars.as_slice() == [key.clone()]
    {
        return Some(existing);
    }
    let pos = InsertPos {
        parent: rest.to_vec(),
        index: last + 1,
    };
    insert_node(flow, &pos, FlowNode::Report(ReportStmt::Vars(vec![key])));
    let mut new = rest.to_vec();
    new.push(last + 1);
    Some(new)
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

/// Set the environment name of one `BASELINE`/`COMPARISON` role reference of a
/// `FOR … IN ENVS` comparison loop at `path`. `baseline` selects the role list;
/// `index` is the position within that list. Only rewrites an `Env(…)` ref (a
/// `FILE(…)` snapshot ref is left unchanged). Returns whether anything changed.
pub(crate) fn set_env_role(
    flow: &mut ReportFlow,
    path: &[usize],
    baseline: bool,
    index: usize,
    name: &str,
) -> bool {
    let Some(FlowNode::ForEnvs {
        clause:
            EnvClause::Roles {
                baseline: b,
                comparisons: c,
                ..
            },
        ..
    }) = node_at_mut(flow, path)
    else {
        return false;
    };
    let list = if baseline { b } else { c };
    match list.get_mut(index) {
        Some(r @ RoleRef::Env(_)) => {
            *r = RoleRef::Env(name.to_string());
            true
        }
        _ => false,
    }
}

/// Set the `AS` alias/name of a reported node at `path`. For a `REPORT REQUEST`
/// the alias is optional, so an empty `text` clears it; for a `REPORT var AS …`
/// or computed column the name is required, so an empty `text` is rejected
/// (returns `false`, leaving the name untouched). Returns whether it changed.
pub(crate) fn set_report_alias(flow: &mut ReportFlow, path: &[usize], text: &str) -> bool {
    let t = text.trim();
    match node_at_mut(flow, path) {
        Some(FlowNode::Report(ReportStmt::Request { alias, .. })) => {
            *alias = (!t.is_empty()).then(|| t.to_string());
            true
        }
        Some(FlowNode::Report(ReportStmt::VarAs { name, .. }))
        | Some(FlowNode::Report(ReportStmt::Computed { name, .. })) => {
            if t.is_empty() {
                return false;
            }
            *name = t.to_string();
            true
        }
        _ => false,
    }
}

/// Set (or clear) the maximum concurrency of the `PARALLEL` modifier on the
/// loop at `path`. `degree: None` means "no explicit limit", which the runner
/// resolves to the prelude's `MAX_PARALLEL` (or the built-in default) — that is
/// the plain `PARALLEL` form. `Some(0)` is rejected, matching the parser, which
/// refuses `PARALLEL(0)` because a zero-wide pool could never run anything.
/// Returns `false` when the node isn't a loop, isn't marked parallel, or the
/// degree is invalid.
pub(crate) fn set_parallel_degree(
    flow: &mut ReportFlow,
    path: &[usize],
    degree: Option<u32>,
) -> bool {
    if degree == Some(0) {
        return false;
    }
    match node_at_mut(flow, path) {
        Some(FlowNode::ForEach { parallel, .. }) | Some(FlowNode::ForEnvs { parallel, .. }) => {
            match parallel {
                Some(spec) => {
                    spec.degree = degree;
                    true
                }
                // Setting a degree on a serial loop would silently make it
                // concurrent — the PARALLEL modifier has to be attached first.
                None => false,
            }
        }
        _ => false,
    }
}

/// Rename the loop variable of the `FOR` at `path` — the inline text box on the
/// loop chip.
///
/// Only a loop that binds a *single* named value can be renamed this way:
/// `FOR f IN FILES "."` and `FOR t IN ENVS …` both can, while a destructuring
/// pattern (`FOR name, url IN …`) or one with a `...` rest cannot, because the
/// chip has one box and there would be no saying which binder it meant. Those
/// keep to the wizard.
///
/// The name is checked against the parser's own identifier rule, so a box left
/// empty or filled with something like `my file` is rejected rather than
/// written out as text that would no longer parse. Returns whether it changed.
pub(crate) fn set_loop_var(flow: &mut ReportFlow, path: &[usize], text: &str) -> bool {
    let t = text.trim();
    if !crate::report::parser::is_ident(t) {
        return false;
    }
    match node_at_mut(flow, path) {
        Some(FlowNode::ForEach { pattern, .. }) => {
            if pattern.rest || pattern.binders.len() != 1 {
                return false;
            }
            match &mut pattern.binders[0] {
                Binder::Named(name) => {
                    if name == t {
                        return false;
                    }
                    *name = t.to_string();
                    true
                }
                // `_` discards the value; renaming it would be introducing a
                // binder, not editing one.
                Binder::Discard => false,
            }
        }
        Some(FlowNode::ForEnvs { var, .. }) => {
            if var == t {
                return false;
            }
            *var = t.to_string();
            true
        }
        _ => false,
    }
}

/// The folder/file a `FOR` loop draws from, when it is a single path the chip
/// can show a picker for: `FILES "dir"`, `FOLDERS "dir"` and `TUPLES FROM
/// "file"`. `None` for every other producer — a list literal, a `ZIP`/`CONCAT`
/// of several, or a named `LIST` have no one path to pick.
pub(crate) fn loop_dir(flow: &ReportFlow, path: &[usize]) -> Option<String> {
    match node_at(flow, path) {
        Some(FlowNode::ForEach { producer, .. }) => match producer {
            Producer::Files { dir, .. } | Producer::Folders { dir, .. } => Some(dir.clone()),
            Producer::Tuples { path } => Some(path.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Point the loop at `path` at a different folder/file — the chip's picker and
/// its inline path box.
///
/// Empty is rejected: a `FILES ""` reads as the process working directory,
/// which is never what clearing a box was meant to ask for. Returns whether it
/// changed.
pub(crate) fn set_loop_dir(flow: &mut ReportFlow, path: &[usize], text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    match node_at_mut(flow, path) {
        Some(FlowNode::ForEach { producer, .. }) => match producer {
            Producer::Files { dir, .. } | Producer::Folders { dir, .. } => {
                if dir == t {
                    return false;
                }
                *dir = t.to_string();
                true
            }
            Producer::Tuples { path } => {
                if path == t {
                    return false;
                }
                *path = t.to_string();
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// Set (or clear, when `text` is blank) the `MATCH "glob"` of a `FILES` loop.
/// Clearing is meaningful here, unlike the folder: a `FILES` with no `MATCH`
/// simply takes every file.
pub(crate) fn set_loop_glob(flow: &mut ReportFlow, path: &[usize], text: &str) -> bool {
    let t = text.trim();
    match node_at_mut(flow, path) {
        Some(FlowNode::ForEach {
            producer: Producer::Files { glob, .. } | Producer::Folders { glob, .. },
            ..
        }) => {
            let next = (!t.is_empty()).then(|| t.to_string());
            if *glob == next {
                return false;
            }
            *glob = next;
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The report's header directives, as an editable list
// ---------------------------------------------------------------------------

/// A header directive as the editors present it: which of the `# key: value`
/// lines exist, how each one is edited, and whether it is worth showing when
/// unset.
///
/// Lives here rather than in either front-end because *both* editors offer the
/// same settings over the same directives, and when this table lived only in
/// the GUI the terminal UI silently fell behind it — it could bind a collection
/// and nothing else. One table, and a directive added to the language shows up
/// in both editors or neither.
pub(crate) struct HeaderSpec {
    pub(crate) key: &'static str,
    /// `true` for the directives worth showing even when unset (as a prompt),
    /// rather than hiding them behind the "add setting" menu.
    pub(crate) always_shown: bool,
    /// `true` when leaving this unset actually stops the report running, so the
    /// prompt is drawn in the error colour. Only `collection:` qualifies:
    /// everything else either has a working default (`output:` falls back to
    /// `csv`, `root:` to the report's folder) or is simply absent.
    pub(crate) required: bool,
    pub(crate) kind: HeaderKind,
}

/// How one header directive is edited.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HeaderKind {
    /// Pick from the open collections.
    Collection,
    /// Pick from the loaded global environments.
    Environment,
    /// Pick one of the writers PaperTrail can produce.
    ///
    /// `# output:` names a *format*, never a filename — the runner derives the
    /// file from the report's own name (only the CLI's `-o` flag takes a path),
    /// and `output_extension_from_header` rejects anything that isn't one of
    /// [`crate::report::writer::OUTPUT_EXTENSIONS`]. So this is a closed list,
    /// and offering a free-text field with a file browser (as this first did)
    /// only invited values the report would refuse to run with.
    Format,
    /// A folder, typed or chosen with the file picker.
    Folder,
    /// A file, typed or chosen with the file picker.
    File,
    /// Free text (the `columns:` list).
    Text,
}

impl HeaderKind {
    /// Whether this directive's value is a filesystem path — the two that are
    /// get a file/folder browser as well as a text field.
    pub(crate) fn is_path(self) -> bool {
        matches!(self, HeaderKind::Folder | HeaderKind::File)
    }
}

/// Every header directive the editors offer, in the order they are shown.
pub(crate) fn header_specs() -> [HeaderSpec; 6] {
    [
        HeaderSpec {
            key: "collection",
            always_shown: true,
            required: true,
            kind: HeaderKind::Collection,
        },
        HeaderSpec {
            key: "output",
            always_shown: true,
            required: false,
            kind: HeaderKind::Format,
        },
        HeaderSpec {
            key: "environment",
            always_shown: false,
            required: false,
            kind: HeaderKind::Environment,
        },
        HeaderSpec {
            key: "root",
            always_shown: false,
            required: false,
            kind: HeaderKind::Folder,
        },
        HeaderSpec {
            key: "baseline",
            always_shown: false,
            required: false,
            kind: HeaderKind::File,
        },
        HeaderSpec {
            key: "columns",
            always_shown: false,
            required: false,
            kind: HeaderKind::Text,
        },
    ]
}

/// The explanation of what one header directive does — the GUI's hover help and
/// the terminal UI's status line, from one place so the two say the same thing.
pub(crate) fn header_help(key: &str, s: &Strings) -> &'static str {
    match key {
        "collection" => s.chip_help_hdr_collection,
        "output" => s.chip_help_hdr_output,
        "environment" => s.chip_help_hdr_environment,
        "root" => s.chip_help_hdr_root,
        "baseline" => s.chip_help_hdr_baseline,
        _ => s.chip_help_hdr_columns,
    }
}

/// The value a freshly-added optional directive starts at.
///
/// Always `?`, the "present but not filled in yet" sentinel every editor here
/// already understands (it renders as the unset prompt). It must not be the
/// empty string: [`set_header`] treats an empty value as *remove this
/// directive*, so an empty placeholder made picking a setting from the add menu
/// do nothing at all — which is exactly what `columns:` used to do.
pub(crate) const HEADER_PLACEHOLDER: &str = "?";

/// Whether a directive's stored value counts as "not filled in yet" — either
/// absent altogether or still holding the [`HEADER_PLACEHOLDER`] sentinel.
pub(crate) fn header_unset(value: &str) -> bool {
    value.is_empty() || value == HEADER_PLACEHOLDER
}

/// Set the `# key: value` header directive, or remove it entirely when `value`
/// is `None` (or blank).
///
/// A directive that already exists is edited in place so the user's own
/// ordering and any interleaved comments survive; a new one is inserted after
/// the last existing directive rather than appended, which keeps the directives
/// together above any trailing comment block.
///
/// Returns `true` when the header actually changed.
pub(crate) fn set_header(flow: &mut ReportFlow, key: &str, value: Option<&str>) -> bool {
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    let existing = flow.header.lines.iter().position(
        |l| matches!(l, HeaderLine::Directive { key: k, .. } if k.eq_ignore_ascii_case(key)),
    );
    match (existing, value) {
        (Some(i), Some(v)) => {
            let HeaderLine::Directive { value: old, .. } = &mut flow.header.lines[i] else {
                return false;
            };
            if old == v {
                return false;
            }
            *old = v.to_string();
            true
        }
        (Some(i), None) => {
            flow.header.lines.remove(i);
            true
        }
        (None, Some(v)) => {
            let at = flow
                .header
                .lines
                .iter()
                .rposition(|l| matches!(l, HeaderLine::Directive { .. }))
                .map_or(0, |i| i + 1);
            flow.header.lines.insert(
                at,
                HeaderLine::Directive {
                    key: key.to_string(),
                    value: v.to_string(),
                },
            );
            true
        }
        (None, None) => false,
    }
}

/// Append a new `WITH` field (a `name: query` column) to the report-request at
/// `path`, returning its index. A no-op (`None`) when the node is not a report
/// request.
pub(crate) fn add_with_field(
    flow: &mut ReportFlow,
    path: &[usize],
    name: &str,
    query: &str,
    stats: Vec<StatKind>,
) -> Option<usize> {
    if let Some(FlowNode::Report(ReportStmt::Request { with, .. })) = node_at_mut(flow, path) {
        with.push(WithItem::Field {
            name: name.to_string(),
            query: query.to_string(),
            stats,
            image: None,
        });
        Some(with.len() - 1)
    } else {
        None
    }
}

/// Overwrite the `name`/`query` of the `WITH` *field* at `index` of the
/// report-request at `path`, preserving any `STATISTICS(…)`. Returns whether it
/// changed (`false` if the node/index is not a `WITH` field).
pub(crate) fn set_with_field(
    flow: &mut ReportFlow,
    path: &[usize],
    index: usize,
    name: &str,
    query: &str,
    stats: Vec<StatKind>,
) -> bool {
    if let Some(FlowNode::Report(ReportStmt::Request { with, .. })) = node_at_mut(flow, path)
        && let Some(WithItem::Field {
            name: n,
            query: q,
            stats: st,
            // The form doesn't edit the `IMAGE(…)` hint, so it is left alone
            // rather than being cleared by an unrelated rename.
            image: _,
        }) = with.get_mut(index)
    {
        *n = name.to_string();
        *q = query.to_string();
        *st = stats;
        true
    } else {
        false
    }
}
/// whole node should now be *removed* — detaching `REPORT` from a reported
/// variable/computed column leaves no valid statement behind (there is no bare
/// variable node), so the caller drops the row entirely.
pub(crate) fn detach_modifier(flow: &mut ReportFlow, path: &[usize], which: DetachWhich) -> bool {
    let Some(node) = node_at_mut(flow, path) else {
        return false;
    };
    detach_from_node(node, which)
}

/// Whether detaching `which` from `node` would leave a statement that still
/// stands on its own.
///
/// This is the rule the block editor uses to decide whether a chip can be
/// pulled out of a line by itself: a clause whose removal would take the whole
/// row with it (`REPORT` on a reported *column*, say — there is no statement
/// left without it) is load-bearing, so grabbing that chip moves the line
/// instead. Answered by probing the real detach on a throwaway clone, so the
/// two can never drift apart.
pub(crate) fn detach_leaves_statement(node: &FlowNode, which: DetachWhich) -> bool {
    !detach_from_node(&mut node.clone(), which)
}

/// A modifier lifted **off a node with the value it was carrying**, so that
/// dropping it somewhere else re-creates it as it was rather than as a fresh
/// default.
///
/// [`Modifier`] describes a modifier in the abstract — it is what the palette
/// hands out, and attaching it seeds a placeholder the user then fills in. That
/// is exactly wrong for a clause pulled off an existing line: dragging
/// `SHOW(Time, Status)` from one reported request to another has to bring
/// `Time, Status` along, or the gesture silently rewrites the user's work.
/// A `CarriedMod` is therefore the modifier *and its contents*.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum CarriedMod {
    Report,
    Parallel(Option<ParallelSpec>),
    As(String),
    /// One `WITH` field. A report request may hold several, so unlike the rest
    /// this one always has room at the destination.
    With(WithItem),
    Response(ResponseFmt),
    Show(Vec<String>),
    Hide(Vec<String>),
    BaselineShow(Vec<String>),
    Role {
        baseline: bool,
        role: RoleRef,
    },
    /// A whole `WITH … END` block.
    WithBlock(Vec<WithItem>),
    Statistics(Vec<StatKind>),
}

/// Read the value the modifier `which` holds on `node`, ready to be grafted
/// onto another node. `None` when `node` doesn't actually carry it.
pub(crate) fn carry_modifier(node: &FlowNode, which: DetachWhich) -> Option<CarriedMod> {
    Some(match (which, node) {
        (DetachWhich::Report, FlowNode::Report(_)) => CarriedMod::Report,
        (
            DetachWhich::Parallel,
            FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. },
        ) => CarriedMod::Parallel(parallel.clone()),
        (DetachWhich::As, FlowNode::Report(ReportStmt::Request { alias, .. })) => {
            CarriedMod::As(alias.clone()?)
        }
        (DetachWhich::As, FlowNode::Report(ReportStmt::VarAs { name, .. })) => {
            CarriedMod::As(name.clone())
        }
        (DetachWhich::With(i), FlowNode::Report(ReportStmt::Request { with, .. })) => {
            CarriedMod::With(with.get(i)?.clone())
        }
        (DetachWhich::Response, FlowNode::Report(ReportStmt::Request { response_fmt, .. })) => {
            CarriedMod::Response(*response_fmt.as_ref()?)
        }
        (DetachWhich::Show, FlowNode::Report(ReportStmt::Request { show, .. })) => {
            CarriedMod::Show(non_empty(show)?)
        }
        (DetachWhich::Hide, FlowNode::Report(ReportStmt::Request { hide, .. })) => {
            CarriedMod::Hide(non_empty(hide)?)
        }
        (
            DetachWhich::BaselineShow,
            FlowNode::ForEnvs {
                clause: EnvClause::Roles { baseline_show, .. },
                ..
            },
        ) => CarriedMod::BaselineShow(non_empty(baseline_show)?),
        (
            DetachWhich::Role { baseline, index },
            FlowNode::ForEnvs {
                clause:
                    EnvClause::Roles {
                        baseline: b,
                        comparisons,
                        ..
                    },
                ..
            },
        ) => CarriedMod::Role {
            baseline,
            role: if baseline { b } else { comparisons }.get(index)?.clone(),
        },
        (DetachWhich::WithBlock, FlowNode::Report(ReportStmt::Request { with, .. })) => {
            CarriedMod::WithBlock(non_empty(with)?)
        }
        (
            DetachWhich::Statistics,
            FlowNode::Report(ReportStmt::VarAs { stats, .. } | ReportStmt::Computed { stats, .. }),
        ) => CarriedMod::Statistics(non_empty(stats)?),
        _ => return None,
    })
}

/// `Some(clone)` for a non-empty list — the "is this clause actually present?"
/// test every list-shaped modifier shares.
fn non_empty<T: Clone>(v: &[T]) -> Option<Vec<T>> {
    (!v.is_empty()).then(|| v.to_vec())
}

impl CarriedMod {
    /// The abstract modifier this is an instance of, when there is one. The
    /// role clauses of an `ENVS` loop (and a whole `WITH` block) have no
    /// palette counterpart, so they answer `None` and carry their own rules.
    fn kind(&self) -> Option<Modifier> {
        Some(match self {
            CarriedMod::Report => Modifier::Report,
            CarriedMod::Parallel(_) => Modifier::Parallel,
            CarriedMod::As(_) => Modifier::As,
            CarriedMod::With(_) => Modifier::With,
            CarriedMod::Response(_) => Modifier::Response,
            CarriedMod::Show(_) => Modifier::Show,
            CarriedMod::Hide(_) => Modifier::Hide,
            CarriedMod::Statistics(_) => Modifier::Statistics,
            CarriedMod::BaselineShow(_) | CarriedMod::Role { .. } | CarriedMod::WithBlock(_) => {
                return None;
            }
        })
    }

    /// Whether this clause can be grafted onto `node`.
    pub(crate) fn applies_to(&self, node: &FlowNode) -> bool {
        match self {
            // A carried REPORT only ever re-wraps a plain request. Dropping it
            // on an assignment *inserts a line* rather than changing this one
            // (see `report_assignment`), which is not a move of the clause in
            // hand, so that stays the palette's job.
            CarriedMod::Report => matches!(node, FlowNode::Request { .. }),
            CarriedMod::BaselineShow(_) => matches!(
                node,
                FlowNode::ForEnvs {
                    clause: EnvClause::Roles { baseline_show, .. },
                    ..
                } if baseline_show.is_empty()
            ),
            // A role joins any comparison loop that doesn't already list it —
            // the point of dragging `COMPARISON(stage)` to another loop.
            CarriedMod::Role { baseline, role } => matches!(
                node,
                FlowNode::ForEnvs {
                    clause: EnvClause::Roles { baseline: b, comparisons, .. },
                    ..
                } if !if *baseline { b } else { comparisons }.contains(role)
            ),
            CarriedMod::WithBlock(_) => matches!(
                node,
                FlowNode::Report(ReportStmt::Request { with, .. }) if with.is_empty()
            ),
            other => other.kind().is_some_and(|m: Modifier| m.applies_to(node)),
        }
    }

    /// Why this clause refuses to graft onto `node`, or `None` when it does.
    pub(crate) fn reject_reason(&self, node: &FlowNode, s: &Strings) -> Option<&'static str> {
        if self.applies_to(node) {
            return None;
        }
        Some(match self {
            CarriedMod::Report => {
                if matches!(node, FlowNode::Report(_)) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_report
                }
            }
            CarriedMod::BaselineShow(_) | CarriedMod::Role { .. } => {
                if matches!(
                    node,
                    FlowNode::ForEnvs {
                        clause: EnvClause::Roles { .. },
                        ..
                    }
                ) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_compare_only
                }
            }
            CarriedMod::WithBlock(_) => {
                if matches!(node, FlowNode::Report(ReportStmt::Request { .. })) {
                    s.mod_reject_present
                } else {
                    s.mod_reject_with
                }
            }
            other => other.kind()?.reject_reason(node, s)?,
        })
    }

    /// Graft this clause onto `node`, keeping the value it was carrying.
    /// Returns whether anything changed.
    pub(crate) fn attach_to(&self, node: &mut FlowNode) -> bool {
        if !self.applies_to(node) {
            return false;
        }
        match self {
            CarriedMod::Report => {
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
            CarriedMod::Parallel(spec) => match node {
                FlowNode::ForEach { parallel, .. } | FlowNode::ForEnvs { parallel, .. } => {
                    *parallel = Some(spec.clone().unwrap_or_default());
                }
                _ => {}
            },
            CarriedMod::As(name) => match node {
                FlowNode::Report(ReportStmt::Request { alias, .. }) => *alias = Some(name.clone()),
                FlowNode::Report(ReportStmt::Vars(vars)) if vars.len() == 1 => {
                    let var = vars.remove(0);
                    *node = FlowNode::Report(ReportStmt::VarAs {
                        var,
                        name: name.clone(),
                        stats: Vec::new(),
                        image: None,
                    });
                }
                _ => {}
            },
            CarriedMod::With(item) => {
                if let FlowNode::Report(ReportStmt::Request { with, .. }) = node {
                    with.push(item.clone());
                }
            }
            CarriedMod::Response(fmt) => {
                if let FlowNode::Report(ReportStmt::Request { response_fmt, .. }) = node {
                    *response_fmt = Some(*fmt);
                }
            }
            CarriedMod::Show(cols) => {
                if let FlowNode::Report(ReportStmt::Request { show, .. }) = node {
                    *show = cols.clone();
                }
            }
            CarriedMod::Hide(cols) => {
                if let FlowNode::Report(ReportStmt::Request { hide, .. }) = node {
                    *hide = cols.clone();
                }
            }
            CarriedMod::BaselineShow(cols) => {
                if let FlowNode::ForEnvs {
                    clause: EnvClause::Roles { baseline_show, .. },
                    ..
                } = node
                {
                    *baseline_show = cols.clone();
                }
            }
            CarriedMod::Role { baseline, role } => {
                if let FlowNode::ForEnvs {
                    clause:
                        EnvClause::Roles {
                            baseline: b,
                            comparisons,
                            ..
                        },
                    ..
                } = node
                {
                    if *baseline { b } else { comparisons }.push(role.clone());
                }
            }
            CarriedMod::WithBlock(items) => {
                if let FlowNode::Report(ReportStmt::Request { with, .. }) = node {
                    *with = items.clone();
                }
            }
            CarriedMod::Statistics(stats) => match node {
                FlowNode::Report(ReportStmt::VarAs { stats: s, .. })
                | FlowNode::Report(ReportStmt::Computed { stats: s, .. }) => *s = stats.clone(),
                _ => {}
            },
        }
        true
    }
}

/// Move (or, with `copy`, clone) the modifier `which` from the node at `from`
/// onto the node at `to`. This is what dropping a clause pulled off one line
/// onto another line does. A no-op returning `false` unless the clause is
/// really there *and* the destination will take it — and the two are never
/// half-applied, so a refused drop leaves the source untouched.
///
/// Detaching first is safe because only clauses whose removal leaves a valid
/// statement can be picked up in the first place (see
/// [`detach_leaves_statement`]), so no row disappears and no path shifts.
pub(crate) fn transfer_modifier(
    flow: &mut ReportFlow,
    from: &[usize],
    which: DetachWhich,
    to: &[usize],
    copy: bool,
) -> bool {
    if from == to {
        return false;
    }
    let Some(carried) = node_at(flow, from).and_then(|n| carry_modifier(n, which)) else {
        return false;
    };
    if !node_at(flow, to).is_some_and(|n| carried.applies_to(n)) {
        return false;
    }
    if !copy {
        detach_modifier(flow, from, which);
    }
    node_at_mut(flow, to).is_some_and(|n| carried.attach_to(n))
}

/// The body of [`detach_modifier`], on an already-resolved node. Returns `true`
/// when nothing coherent is left and the caller should remove the row.
fn detach_from_node(node: &mut FlowNode, which: DetachWhich) -> bool {
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
        DetachWhich::BaselineShow => {
            if let FlowNode::ForEnvs {
                clause: EnvClause::Roles { baseline_show, .. },
                ..
            } = node
            {
                baseline_show.clear();
            }
            false
        }
        DetachWhich::Role { baseline, index } => {
            if let FlowNode::ForEnvs { clause, .. } = node
                && let EnvClause::Roles {
                    baseline: b,
                    comparisons,
                    ..
                } = clause
            {
                let side = if baseline { &mut *b } else { &mut *comparisons };
                if index < side.len() {
                    side.remove(index);
                }
                // A comparison needs both halves. Emptying either one leaves
                // nothing to compare against, so the loop degrades to a plain
                // pass over whichever environments are left rather than
                // serializing a half-written `BASELINE(…)` with no
                // `COMPARISON(…)` (which would not re-parse). Snapshot refs
                // have no plain form and so drop out.
                if b.is_empty() || comparisons.is_empty() {
                    let names: Vec<String> = b
                        .iter()
                        .chain(comparisons.iter())
                        .filter_map(|r| match r {
                            RoleRef::Env(n) => Some(n.clone()),
                            RoleRef::File(_) => None,
                        })
                        .collect();
                    *clause = EnvClause::Plain(names);
                }
            }
            false
        }
        DetachWhich::WithBlock => {
            if let FlowNode::Report(ReportStmt::Request { with, .. }) = node {
                with.clear();
            }
            false
        }
        DetachWhich::Statistics => {
            match node {
                FlowNode::Report(ReportStmt::VarAs { stats, .. })
                | FlowNode::Report(ReportStmt::Computed { stats, .. }) => stats.clear(),
                _ => {}
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

    /// The loop chip's inline name box: it renames the one thing a single-binder
    /// loop binds, and refuses anything the parser would not take back.
    #[test]
    fn set_loop_var_renames_a_single_binder_and_rejects_names_that_would_not_parse() {
        let mut f = flow("FOR file IN FILES \".\" MATCH \"*.json\"\n  REQUEST A\nEND\n");
        assert!(set_loop_var(&mut f, &[0], "doc"));
        assert!(
            f.to_text().contains("FOR doc IN FILES"),
            "the rename reached the source: {}",
            f.to_text()
        );

        assert!(
            !set_loop_var(&mut f, &[0], "doc"),
            "no change is not a change"
        );
        for bad in ["", "   ", "my file", "2fast", "a-b"] {
            assert!(
                !set_loop_var(&mut f, &[0], bad),
                "{bad:?} is not an identifier and must be refused"
            );
        }
        assert!(
            f.to_text().contains("FOR doc IN FILES"),
            "and a refused name leaves the loop alone"
        );

        // An ENVS loop binds one name too, so it renames the same way.
        let mut e = flow("FOR t IN ENVS BASELINE(\"prod\")\n  REQUEST A\nEND\n");
        assert!(set_loop_var(&mut e, &[0], "target"));
        assert!(e.to_text().contains("FOR target IN ENVS"));
    }

    /// A destructuring loop has more than one name, and the chip has one box --
    /// there would be no saying which binder it meant, so it stays with the
    /// wizard rather than guessing.
    #[test]
    fn set_loop_var_refuses_a_pattern_that_binds_more_than_one_name() {
        let mut f = flow("FOR (NAME, URL) IN DOCS\n  REQUEST A\nEND\n");
        assert!(!set_loop_var(&mut f, &[0], "x"));
        assert!(
            f.to_text().contains("FOR (NAME, URL) IN"),
            "the pattern is untouched: {}",
            f.to_text()
        );

        // A `...` rest binds an unknown number of positions, so one box can
        // speak for none of them either.
        let mut r = flow("FOR (HEAD, ...) IN DOCS\n  REQUEST A\nEND\n");
        assert!(!set_loop_var(&mut r, &[0], "x"));

        // `_` discards its position; renaming it would be adding a binder.
        let mut d = flow("FOR _ IN FILES \".\"\n  REQUEST A\nEND\n");
        assert!(!set_loop_var(&mut d, &[0], "x"));
    }

    /// The folder box and its picker, over the three producers that have one
    /// path to point at.
    #[test]
    fn the_loop_folder_can_be_read_and_repointed_for_the_producers_that_have_one() {
        let mut files = flow("FOR f IN FILES \"cases\" MATCH \"*.json\"\n  REQUEST A\nEND\n");
        assert_eq!(loop_dir(&files, &[0]).as_deref(), Some("cases"));
        assert!(set_loop_dir(&mut files, &[0], "other/cases"));
        assert!(files.to_text().contains("FILES \"other/cases\""));
        assert!(
            !set_loop_dir(&mut files, &[0], "   "),
            "clearing the folder would silently mean the working directory"
        );

        let mut folders = flow("FOR d IN FOLDERS \"envs\"\n  REQUEST A\nEND\n");
        assert_eq!(loop_dir(&folders, &[0]).as_deref(), Some("envs"));
        assert!(set_loop_dir(&mut folders, &[0], "environments"));
        assert!(folders.to_text().contains("FOLDERS \"environments\""));

        let mut tuples = flow("FOR t IN TUPLES FROM \"rows.csv\"\n  REQUEST A\nEND\n");
        assert_eq!(loop_dir(&tuples, &[0]).as_deref(), Some("rows.csv"));
        assert!(set_loop_dir(&mut tuples, &[0], "data/rows.csv"));
        assert!(tuples.to_text().contains("TUPLES FROM \"data/rows.csv\""));

        // A list literal has no single path, so the chip shows no picker.
        let list = flow("FOR x IN [\"a\", \"b\"]\n  REQUEST A\nEND\n");
        assert_eq!(loop_dir(&list, &[0]), None);
    }

    /// Unlike the folder, an empty glob is a real answer: `FILES` with no
    /// `MATCH` takes every file.
    #[test]
    fn the_loop_glob_can_be_set_and_cleared() {
        let mut f = flow("FOR f IN FILES \"cases\"\n  REQUEST A\nEND\n");
        assert!(!f.to_text().contains("MATCH"));
        assert!(set_loop_glob(&mut f, &[0], "*.json"));
        assert!(f.to_text().contains("MATCH \"*.json\""));

        assert!(set_loop_glob(&mut f, &[0], ""));
        assert!(
            !f.to_text().contains("MATCH"),
            "clearing the box drops the clause: {}",
            f.to_text()
        );
    }

    #[test]
    fn set_parallel_degree_edits_the_concurrency_limit_and_rejects_zero() {
        let mut f = flow("PARALLEL FOR X IN FILES \"/d\"\n    REQUEST A\nEND\n");

        assert!(set_parallel_degree(&mut f, &[0], Some(4)));
        assert!(f.to_text().contains("PARALLEL(4) FOR"));

        // Clearing the degree goes back to the plain PARALLEL form, where the
        // limit comes from the prelude rather than the loop.
        assert!(set_parallel_degree(&mut f, &[0], None));
        assert!(f.to_text().contains("PARALLEL FOR"));

        // The parser refuses PARALLEL(0), so the editor must too — otherwise a
        // saved flow wouldn't load back.
        assert!(set_parallel_degree(&mut f, &[0], Some(4)));
        assert!(!set_parallel_degree(&mut f, &[0], Some(0)));
        assert!(f.to_text().contains("PARALLEL(4) FOR"));
    }

    #[test]
    fn set_header_adds_edits_and_removes_directives() {
        let mut f = flow(
            "# collection: api.hurl
REQUEST A
",
        );

        // Editing in place keeps the directive where the user put it.
        assert!(set_header(&mut f, "collection", Some("other.hurl")));
        assert!(f.to_text().contains("# collection: other.hurl"));
        assert_eq!(f.header.collection(), Some("other.hurl"));

        // A new directive lands with the others, not at the top of the file.
        assert!(set_header(&mut f, "output", Some("out.csv")));
        assert_eq!(f.header.output(), Some("out.csv"));
        let text = f.to_text();
        assert!(
            text.find("# collection:") < text.find("# output:"),
            "new directives are appended after the existing ones: {text:?}"
        );

        // Setting the same value again is not a change, so it can't push a
        // pointless undo entry or mark the report dirty.
        assert!(!set_header(&mut f, "output", Some("out.csv")));

        // Clearing removes the line rather than leaving `# output:` empty,
        // which the parser would read as a directive set to the empty string.
        assert!(set_header(&mut f, "output", None));
        assert_eq!(f.header.output(), None);
        assert!(!f.to_text().contains("# output"));
        assert!(!set_header(&mut f, "output", None));

        // Blank input means "unset", not "set to nothing".
        assert!(!set_header(&mut f, "root", Some("   ")));
        assert_eq!(f.header.root(), None);
    }

    /// Free-form `#` comments are the user's own notes; editing a directive must
    /// never reorder or drop them.
    #[test]
    fn set_header_leaves_free_form_comments_alone() {
        let mut f = flow(
            "# collection: api.hurl
# a note to self
REQUEST A
",
        );
        assert!(set_header(&mut f, "environment", Some("dev")));
        let text = f.to_text();
        assert!(text.contains("# a note to self"), "{text:?}");
        assert!(
            text.find("# environment:") < text.find("# a note"),
            "the new directive joins the directive block, above the notes: {text:?}"
        );
    }

    #[test]
    fn a_degree_cannot_be_set_on_a_loop_that_is_not_parallel() {
        // Accepting this would silently turn a serial loop concurrent; the
        // PARALLEL modifier has to be attached first.
        let mut f = flow("FOR X IN FILES \"/d\"\n    REQUEST A\nEND\n");
        assert!(!set_parallel_degree(&mut f, &[0], Some(2)));
        assert!(!f.to_text().contains("PARALLEL"));

        // Nor on a node that has no PARALLEL concept at all.
        let mut g = flow("REQUEST A\n");
        assert!(!set_parallel_degree(&mut g, &[0], Some(2)));
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
        // REPORT now applies to a plain assignment (dropping it inserts a
        // sibling `REPORT (VAR)` line — see `report_assignment`); the loop /
        // request-only modifiers still do not.
        assert!(Modifier::Report.applies_to(n));
        assert!(!Modifier::Parallel.applies_to(n));
        assert!(!Modifier::With.applies_to(n));
        assert!(!Modifier::As.applies_to(n));
    }

    /// A clause is only pullable-out if the statement survives without it.
    /// `REPORT` is load-bearing on a reported column (there is no statement left
    /// at all) but not on a reported request, which falls back to a plain send.
    #[test]
    fn report_is_load_bearing_on_a_column_but_not_on_a_request() {
        let flow =
            parse_flow("REPORT REQUEST A\nREPORT TIER AS Plan\nREPORT \"x\" AS c\nREPORT (A, B)\n")
                .expect("fixture parses");
        assert!(
            detach_leaves_statement(&flow.nodes[0], DetachWhich::Report),
            "a reported request downgrades to a plain REQUEST, so REPORT snaps off"
        );
        for (i, what) in [
            (1, "REPORT … AS"),
            (2, "a computed column"),
            (3, "REPORT (…)"),
        ] {
            assert!(
                !detach_leaves_statement(&flow.nodes[i], DetachWhich::Report),
                "nothing is left of {what} without REPORT, so it must move the whole row"
            );
        }
    }

    /// STATISTICS needs a named column to summarise; a bare `REPORT (A, B)` has
    /// no single column to attach it to, and it is refused with a reason.
    #[test]
    fn statistics_attaches_to_a_named_column_and_only_once() {
        let mut flow = parse_flow("REPORT TIER AS Plan\nREPORT (A, B)\n").expect("fixture parses");
        assert!(
            attach_modifier(&mut flow, &[0], Modifier::Statistics),
            "a named column accepts STATISTICS"
        );
        assert!(
            flow.to_text().contains("STATISTICS("),
            "the clause is written out: {}",
            flow.to_text()
        );
        assert!(
            !attach_modifier(&mut flow, &[0], Modifier::Statistics),
            "a column that already has STATISTICS refuses a second one"
        );
        assert!(
            !attach_modifier(&mut flow, &[1], Modifier::Statistics),
            "REPORT (A, B) names no single column"
        );
        detach_modifier(&mut flow, &[0], DetachWhich::Statistics);
        assert!(
            !flow.to_text().contains("STATISTICS(") && flow.to_text().contains("AS Plan"),
            "detaching leaves the column itself alone: {}",
            flow.to_text()
        );
    }

    /// A `WITH` field is the report column a request actually names, so it is
    /// what STATISTICS attaches to — the request line above summarises nothing.
    /// The block editor could show a field's STATISTICS but had no way to add
    /// one, because a field isn't a node a modifier can be dropped on.
    #[test]
    fn statistics_attaches_to_a_with_field() {
        let mut flow =
            parse_flow("REPORT REQUEST svc WITH\n    Elapsed: Time\n    RESPONSE RAW\nEND\n")
                .expect("fixture parses");
        // The request line itself still refuses it: its columns are its fields.
        assert!(
            !attach_modifier(&mut flow, &[0], Modifier::Statistics),
            "a report request names no single column"
        );

        assert!(
            with_stats_applies(with_of(&flow), 0),
            "a named field takes it"
        );
        assert!(attach_with_stats(&mut flow, &[0], 0));
        assert!(
            flow.to_text().contains("Elapsed: Time STATISTICS(COUNT)"),
            "the clause lands on the field: {}",
            flow.to_text()
        );

        // Only once, and never on a bare `WITH RESPONSE` item (which has no name
        // to put a column under) or a field that isn't there.
        assert!(!with_stats_applies(with_of(&flow), 0), "already has one");
        assert!(!attach_with_stats(&mut flow, &[0], 0));
        assert!(
            !with_stats_applies(with_of(&flow), 1),
            "RESPONSE RAW is not a column"
        );
        assert!(!attach_with_stats(&mut flow, &[0], 1));
        assert!(!attach_with_stats(&mut flow, &[0], 9), "no such field");

        // And it round-trips back through the parser as a field clause.
        let again = parse_flow(&flow.to_text()).expect("reparses");
        assert_eq!(again.to_text(), flow.to_text());
    }

    /// The `WITH` items of the report request at the root of `flow`.
    fn with_of(flow: &ReportFlow) -> &[WithItem] {
        match &flow.nodes[0] {
            FlowNode::Report(ReportStmt::Request { with, .. }) => with,
            other => panic!("expected a report request, got {other:?}"),
        }
    }

    /// Every refusal has to distinguish "wrong kind of block" from "it's
    /// already there" — telling someone REPORT only goes on a request while
    /// they hover a reported request is worse than saying nothing.
    #[test]
    fn a_duplicate_modifier_is_refused_as_a_duplicate_not_as_a_wrong_block() {
        let flow = parse_flow("REPORT REQUEST A\nREQUEST B\nREPORT TIER AS Plan\n")
            .expect("fixture parses");
        let s = Strings::english();

        assert_eq!(
            Modifier::Report.reject_reason(&flow.nodes[0], s),
            Some(s.mod_reject_present),
            "an already-reported request has REPORT, it isn't the wrong shape for it"
        );
        assert_eq!(
            Modifier::Report.reject_reason(&flow.nodes[2], s),
            Some(s.mod_reject_present),
            "a reported column is a REPORT statement too"
        );
        assert_eq!(
            Modifier::Report.reject_reason(&flow.nodes[1], s),
            None,
            "a plain request still takes REPORT"
        );

        // The same clause carried off another line answers the same way.
        let carried = carry_modifier(&flow.nodes[0], DetachWhich::Report).expect("carries REPORT");
        assert_eq!(
            carried.reject_reason(&flow.nodes[0], s),
            Some(s.mod_reject_present)
        );
    }

    /// The whole point of dragging a clause between lines: it has to arrive
    /// with the value it left with, not as a fresh placeholder.
    #[test]
    fn a_show_dragged_to_another_request_brings_its_columns_with_it() {
        let mut flow = parse_flow("REPORT REQUEST A SHOW(Time, HttpStatus)\nREPORT REQUEST B\n")
            .expect("fixture parses");

        assert!(
            transfer_modifier(&mut flow, &[0], DetachWhich::Show, &[1], false),
            "an as-yet SHOW-less reported request accepts the clause"
        );
        let text = flow.to_text();
        assert!(
            text.contains("REQUEST B SHOW(Time, HttpStatus)"),
            "the columns travel with the clause: {text}"
        );
        assert!(
            !text.contains("REQUEST A SHOW"),
            "a move leaves nothing behind on the source line: {text}"
        );

        // The destination already has one now, so dragging it back is refused
        // outright rather than half-applied (the source must survive intact).
        assert!(
            !transfer_modifier(&mut flow, &[1], DetachWhich::Show, &[1], false),
            "a line never transfers a clause to itself"
        );
    }

    /// Shift-dropping copies instead of moving, which is how one loop's
    /// `PARALLEL(4)` gets cloned onto its neighbours.
    #[test]
    fn a_copied_parallel_clones_its_degree_and_leaves_the_original_alone() {
        let mut flow = parse_flow(
            "PARALLEL(4) FOR X IN FILES \"/a\"\n    REQUEST A\nEND\nFOR Y IN FILES \"/b\"\n    REQUEST B\nEND\n",
        )
        .expect("fixture parses");

        assert!(
            transfer_modifier(&mut flow, &[0], DetachWhich::Parallel, &[1], true),
            "a plain loop accepts a copied PARALLEL"
        );
        let text = flow.to_text();
        assert_eq!(
            text.matches("PARALLEL(4)").count(),
            2,
            "a copy keeps the original and reproduces its degree: {text}"
        );

        // Moving it onto a loop that now has one is refused, so the source keeps
        // its own clause rather than losing it to a drop that did nothing.
        assert!(
            !transfer_modifier(&mut flow, &[0], DetachWhich::Parallel, &[1], false),
            "a loop that is already parallel takes no second PARALLEL"
        );
        assert!(
            flow.to_text().matches("PARALLEL(4)").count() == 2,
            "a refused transfer is not half-applied: {}",
            flow.to_text()
        );
    }

    /// A clause is only offered where it makes sense, and the refusal says why.
    #[test]
    fn a_carried_clause_is_refused_by_a_block_that_cannot_hold_it() {
        let flow = parse_flow("REPORT REQUEST A SHOW(Time)\nK = \"v\"\n").expect("parses");
        let carried =
            carry_modifier(&flow.nodes[0], DetachWhich::Show).expect("the SHOW is really there");
        let s = Strings::english();

        assert!(!carried.applies_to(&flow.nodes[1]), "SET has no columns");
        assert_eq!(
            carried.reject_reason(&flow.nodes[1], s),
            Some(s.mod_reject_request_only),
            "the refusal names the kind of block that would take it"
        );

        // Nothing to carry when the clause isn't on the node at all.
        assert!(
            carry_modifier(&flow.nodes[1], DetachWhich::Show).is_none(),
            "a node without the clause carries nothing"
        );
    }

    /// The baseline's `SHOW(…)` is a chip of its own, so dragging it out has to
    /// clear only that clause and leave the comparison intact.
    #[test]
    fn detaching_the_baseline_show_leaves_the_rest_of_the_compare_loop() {
        let mut flow = parse_flow(
            "FOR E IN ENVS BASELINE(\"prod\") SHOW(Time), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        )
        .expect("fixture parses");
        assert!(
            !detach_modifier(&mut flow, &[0], DetachWhich::BaselineShow),
            "clearing SHOW never removes the loop itself"
        );
        let text = flow.to_text();
        assert!(
            !text.contains("SHOW(") && text.contains("BASELINE(") && text.contains("COMPARISON("),
            "only the SHOW clause goes: {text}"
        );
    }

    /// Dropping either half of a comparison leaves nothing to compare against,
    /// so the loop has to degrade to a plain pass rather than serialize a
    /// `BASELINE(…)` with no `COMPARISON(…)` (which would not re-parse).
    #[test]
    fn detaching_a_comparison_role_degrades_the_loop_to_a_plain_pass() {
        let mut flow = parse_flow(
            "FOR E IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        )
        .expect("fixture parses");
        detach_modifier(
            &mut flow,
            &[0],
            DetachWhich::Role {
                baseline: false,
                index: 0,
            },
        );
        let text = flow.to_text();
        assert!(
            !text.contains("COMPARISON(") && !text.contains("BASELINE(") && text.contains("prod"),
            "the surviving environment is still iterated: {text}"
        );
        assert!(
            parse_flow(&text).is_ok(),
            "the degraded loop re-parses: {text}"
        );
    }

    #[test]
    fn a_refused_modifier_says_whether_the_block_is_wrong_or_the_clause_is_already_there() {
        let s = crate::i18n::Strings::english();
        // Wrong kind of block: PARALLEL on an assignment names what it *does*
        // take, so the user can aim somewhere useful.
        let assign = flow("k = v\n");
        assert_eq!(
            Modifier::Parallel.reject_reason(node_at(&assign, &[0]).unwrap(), s),
            Some(s.mod_reject_parallel),
            "PARALLEL on an assignment should point at FOR loops"
        );
        // Right kind of block, clause already present: the reason must not
        // claim the block is wrong, or the user will move the chip elsewhere.
        let looped = flow(
            "PARALLEL FOR E IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        );
        assert_eq!(
            Modifier::Parallel.reject_reason(node_at(&looped, &[0]).unwrap(), s),
            Some(s.mod_reject_present),
            "an already-parallel loop should say the clause is already there"
        );
        // And an accepted drop has no reason at all.
        let plain =
            flow("FOR E IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n    REQUEST A\nEND\n");
        assert_eq!(
            Modifier::Parallel.reject_reason(node_at(&plain, &[0]).unwrap(), s),
            None,
            "a modifier that applies should give no refusal reason"
        );
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
    fn response_show_hide_modifiers_attach_defaults_and_round_trip() {
        let mut f = flow("REPORT REQUEST analyze\n");
        // Each applies to a bare report request…
        assert!(Modifier::Response.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(Modifier::Show.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(Modifier::Hide.applies_to(node_at(&f, &[0]).unwrap()));
        // …and drops a sensible default in place.
        assert!(attach_modifier(&mut f, &[0], Modifier::Response));
        assert!(attach_modifier(&mut f, &[0], Modifier::Show));
        assert!(attach_modifier(&mut f, &[0], Modifier::Hide));
        // Now attached, none applies a second time (no silent overwrite).
        assert!(!Modifier::Response.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(!Modifier::Show.applies_to(node_at(&f, &[0]).unwrap()));
        assert!(!Modifier::Hide.applies_to(node_at(&f, &[0]).unwrap()));
        // The serialized text re-parses with the same clauses intact.
        let reparsed = flow(&f.to_text());
        match node_at(&reparsed, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request {
                response_fmt,
                show,
                hide,
                ..
            })) => {
                assert_eq!(*response_fmt, Some(ResponseFmt::Pretty));
                assert_eq!(show, &vec!["HttpStatus".to_string()]);
                assert_eq!(hide, &vec!["HttpStatus".to_string()]);
            }
            other => panic!("expected a decorated report request, got {other:?}"),
        }
    }

    #[test]
    fn report_computed_kind_template_round_trips() {
        // The palette's computed-column template must round-trip through the
        // serializer/parser or dropping it would kick the user out of the editor.
        let node = NodeKind::ReportComputed
            .template()
            .expect("computed kind has a template");
        let mut f = flow("REQUEST A\n");
        insert_node(
            &mut f,
            &InsertPos {
                parent: Vec::new(),
                index: 1,
            },
            node,
        );
        let reparsed = flow(&f.to_text());
        match node_at(&reparsed, &[1]) {
            Some(FlowNode::Report(ReportStmt::Computed { template, name, .. })) => {
                assert!(!template.is_empty());
                assert!(!name.is_empty());
            }
            other => panic!("expected a computed column, got {other:?}"),
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

    #[test]
    fn report_assignment_inserts_a_sibling_report_after_the_set() {
        let mut f = flow("TOKEN=abc\nREQUEST A\n");
        let new = report_assignment(&mut f, &[0]).expect("assign is reportable");
        // A new REPORT (TOKEN) lands right after the assignment.
        assert_eq!(new, vec![1]);
        assert!(matches!(&f.nodes[0], FlowNode::Assign { key, .. } if key == "TOKEN"));
        match &f.nodes[1] {
            FlowNode::Report(ReportStmt::Vars(vars)) => {
                assert_eq!(vars, &vec!["TOKEN".to_string()])
            }
            other => panic!("expected REPORT (TOKEN), got {other:?}"),
        }
        // The assignment survives (it still defines the variable), and the
        // request that followed is pushed down by one.
        assert!(matches!(&f.nodes[2], FlowNode::Request { .. }));
    }

    #[test]
    fn report_assignment_is_a_no_op_on_a_non_assignment() {
        let mut f = flow("REQUEST A\n");
        assert!(report_assignment(&mut f, &[0]).is_none());
        assert_eq!(f.nodes.len(), 1);
    }

    #[test]
    fn report_assignment_is_idempotent_when_already_reported() {
        let mut f = flow("TOKEN=abc\n");
        let first = report_assignment(&mut f, &[0]).expect("assign is reportable");
        assert_eq!(first, vec![1]);
        assert_eq!(f.nodes.len(), 2);
        // Dropping REPORT again selects the existing report line instead of
        // stacking a duplicate column.
        let again = report_assignment(&mut f, &[0]).expect("still reportable");
        assert_eq!(again, vec![1]);
        assert_eq!(f.nodes.len(), 2);
    }

    #[test]
    fn set_env_role_rewrites_one_live_environment_reference() {
        let mut f =
            flow("FOR E IN ENVS BASELINE(\"prod\"), COMPARISON(\"stage\")\n    REQUEST A\nEND\n");
        // Repoint the comparison env; the baseline is untouched.
        assert!(set_env_role(&mut f, &[0], false, 0, "canary"));
        match &f.nodes[0] {
            FlowNode::ForEnvs {
                clause:
                    EnvClause::Roles {
                        baseline,
                        comparisons,
                        ..
                    },
                ..
            } => {
                assert_eq!(baseline, &vec![RoleRef::Env("prod".into())]);
                assert_eq!(comparisons, &vec![RoleRef::Env("canary".into())]);
            }
            other => panic!("expected an ENVS compare loop, got {other:?}"),
        }
    }

    #[test]
    fn set_env_role_leaves_file_snapshots_and_plain_loops_alone() {
        // A FILE(…) snapshot ref is not a live env name, so it is not rewritten.
        let mut f = flow(
            "FOR E IN ENVS BASELINE(FILE(\"snap.baseline\")), COMPARISON(\"stage\")\n    REQUEST A\nEND\n",
        );
        assert!(!set_env_role(&mut f, &[0], true, 0, "prod"));
        // A plain (non-compare) ENVS loop has no role lists to edit.
        let mut g = flow("FOR E IN ENVS \"dev\", \"prod\"\n    REQUEST A\nEND\n");
        assert!(!set_env_role(&mut g, &[0], false, 0, "stage"));
    }

    #[test]
    fn set_report_alias_sets_clears_and_requires() {
        // A report request's alias is optional: set, then clear with "".
        let mut f = flow("REPORT REQUEST analyze AS Result\n");
        assert!(set_report_alias(&mut f, &[0], "Renamed"));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::Report(ReportStmt::Request { alias: Some(a), .. })) if a == "Renamed"
        ));
        assert!(set_report_alias(&mut f, &[0], "   "));
        assert!(matches!(
            node_at(&f, &[0]),
            Some(FlowNode::Report(ReportStmt::Request { alias: None, .. }))
        ));

        // A reported-variable column's name is required: empty is rejected.
        let mut g = flow("REPORT userId AS Id\n");
        assert!(set_report_alias(&mut g, &[0], "UserId"));
        assert!(matches!(
            node_at(&g, &[0]),
            Some(FlowNode::Report(ReportStmt::VarAs { name, .. })) if name == "UserId"
        ));
        assert!(!set_report_alias(&mut g, &[0], ""));
        assert!(matches!(
            node_at(&g, &[0]),
            Some(FlowNode::Report(ReportStmt::VarAs { name, .. })) if name == "UserId"
        ));
    }

    #[test]
    fn add_and_set_with_field_edit_the_with_block() {
        let mut f = flow("REPORT REQUEST analyze RESPONSE PRETTY\n");
        // Append two fields; indices come back in order.
        assert_eq!(
            add_with_field(&mut f, &[0], "Status", "HttpStatus", Vec::new()),
            Some(0)
        );
        assert_eq!(
            add_with_field(&mut f, &[0], "Body", "jsonpath \"$.x\"", Vec::new()),
            Some(1)
        );
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request { with, .. })) => assert_eq!(with.len(), 2),
            other => panic!("expected a report request, got {other:?}"),
        }
        // Rewrite the first field's name/query/statistics in place.
        assert!(set_with_field(
            &mut f,
            &[0],
            0,
            "Code",
            "HttpStatus",
            vec![StatKind::Count, StatKind::Mean],
        ));
        match node_at(&f, &[0]) {
            Some(FlowNode::Report(ReportStmt::Request { with, .. })) => {
                assert!(matches!(
                    &with[0],
                    WithItem::Field {
                        name, query, stats, ..
                    }
                        if name == "Code"
                            && query == "HttpStatus"
                            && stats == &[StatKind::Count, StatKind::Mean]
                ));
            }
            other => panic!("expected a report request, got {other:?}"),
        }
        assert!(f.to_text().contains("STATISTICS(COUNT, MEAN)"));

        // Clearing the checklist drops the clause entirely.
        assert!(set_with_field(
            &mut f,
            &[0],
            0,
            "Code",
            "HttpStatus",
            Vec::new()
        ));
        assert!(!f.to_text().contains("STATISTICS"));

        // A non-request node has no WITH block to add to.
        let mut g = flow("REPORT userId\n");
        assert_eq!(add_with_field(&mut g, &[0], "X", "Y", Vec::new()), None);
        assert!(!set_with_field(&mut g, &[0], 0, "X", "Y", Vec::new()));
    }
}
