//! The structured ("node") editor for PaperTrail report flows — the TUI-native
//! realisation of the "Scratch-like" authoring goal.
//!
//! A report flow is a linear/nested list of statements (an outline), so instead
//! of a mouse-driven block canvas (which fits a GUI, not a terminal) the node
//! editor renders the flow's [`ReportFlow`] AST as a **navigable outline** and
//! lets the user assemble it by inserting / removing / moving whole *nodes*
//! rather than typing text. It delivers Scratch's real goals natively — you can
//! only build valid structures, request names come from a picker seeded by the
//! bound collection, and the node kinds are discoverable from a palette.
//!
//! Both editor views (this one and the source text editor in `reports.rs`) are
//! front-ends over the *same* AST: every structural edit re-serializes the AST
//! back into `report.text` via [`ReportFlow::to_text`], so the two round-trip
//! and a future GUI can reuse the AST and these helpers unchanged.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::{Overlay, PromptKind, TuiApp};
use super::draw::panel;
use super::editor::Editor;
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::flow::{
    EnvClause, FlowNode, Pattern, Producer, ReportFlow, ReportStmt, ResponseFmt, WithItem,
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
fn loop_producer_dir(node: &FlowNode) -> Option<&str> {
    match node {
        FlowNode::ForEach {
            producer: Producer::Files { dir, .. } | Producer::Folders { dir, .. },
            ..
        } => Some(dir),
        _ => None,
    }
}

/// Mutable counterpart to [`loop_producer_dir`].
fn loop_producer_dir_mut(node: &mut FlowNode) -> Option<&mut String> {
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

fn node_at<'a>(flow: &'a ReportFlow, path: &[usize]) -> Option<&'a FlowNode> {
    let (last, rest) = path.split_last()?;
    let mut body = &flow.nodes;
    for &i in rest {
        body = loop_body(body.get(i)?)?;
    }
    body.get(*last)
}

fn node_at_mut<'a>(flow: &'a mut ReportFlow, path: &[usize]) -> Option<&'a mut FlowNode> {
    let (last, rest) = path.split_last()?;
    let body = body_at_mut(flow, rest)?;
    body.get_mut(*last)
}

fn insert_node(flow: &mut ReportFlow, pos: &InsertPos, node: FlowNode) {
    if let Some(body) = body_at_mut(flow, &pos.parent) {
        let idx = pos.index.min(body.len());
        body.insert(idx, node);
    }
}

fn remove_node(flow: &mut ReportFlow, path: &[usize]) -> bool {
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
fn move_node(flow: &mut ReportFlow, path: &[usize], up: bool) -> Option<Vec<usize>> {
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
fn replace_node(flow: &mut ReportFlow, path: &[usize], mut new_node: FlowNode) -> bool {
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
fn parse_one_node(text: &str, prefer_loop: bool) -> Option<FlowNode> {
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
// The insert / request-pick menu
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
    fn label(self, s: &Strings) -> &'static str {
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
    fn needs_request(self) -> bool {
        matches!(self, NodeKind::Request | NodeKind::ReportRequest)
    }

    /// A template node with placeholder fields (for the non-request kinds); the
    /// user then fills the fields in via the "edit as line" prompt.
    fn template(self) -> Option<FlowNode> {
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
                    baseline: vec!["baseline".into()],
                    comparisons: vec!["candidate".into()],
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
fn request_node(name: &str, report: bool) -> FlowNode {
    if report {
        FlowNode::Report(ReportStmt::Request {
            name: name.to_string(),
            alias: None,
            response_fmt: None,
            show: Vec::new(),
            with: Vec::new(),
        })
    } else {
        FlowNode::Request {
            name: name.to_string(),
        }
    }
}

/// The two-step insert/pick palette overlay ([`Overlay::ReportNodeMenu`]).
pub(crate) struct NodeMenu {
    pub(crate) step: NodeMenuStep,
    /// The rows shown: node-kind labels in `PickKind`, request titles in
    /// `PickRequest`.
    pub(crate) options: Vec<String>,
    pub(crate) selected: usize,
    /// Where a newly created node is inserted (ignored when `edit_path` is set).
    pub(crate) pos: InsertPos,
    /// The report being edited (looked up by id so a tab reorder can't misroute).
    pub(crate) report_id: u64,
    /// In `PickRequest`: whether we're building a `REPORT REQUEST` (`true`) or a
    /// plain `REQUEST` (`false`).
    pub(crate) report_kind: bool,
    /// When `Some`, we're changing an existing request node's name at this path
    /// rather than inserting a new node.
    pub(crate) edit_path: Option<Vec<usize>>,
}

/// Which step the [`NodeMenu`] is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NodeMenuStep {
    /// Choosing a node kind to insert.
    PickKind,
    /// Choosing a request name (for `REQUEST` / `REPORT REQUEST`).
    PickRequest,
}

impl NodeMenu {
    /// The overlay title for the current step.
    pub(crate) fn title<'a>(&self, s: &'a Strings) -> &'a str {
        match self.step {
            NodeMenuStep::PickKind => s.node_menu_title,
            NodeMenuStep::PickRequest => s.node_pick_request_title,
        }
    }
}

/// One selectable field in the reported-request form's field checklist.
pub(crate) struct ShowRow {
    pub(crate) name: String,
    pub(crate) included: bool,
}

/// One visible row of a [`RequestForm`]. The layout is dynamic: a plain
/// `REQUEST` shows only Name + Report; ticking Report reveals the reporting
/// options (response format, alias, and the field checklist).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormRow {
    /// The request name (cycles through the bound collection's request titles).
    Name,
    /// The `REPORT` toggle — off = plain `REQUEST`, on = `REPORT REQUEST`.
    Report,
    /// The `RESPONSE RAW/PRETTY` override (only when reporting).
    Response,
    /// The `AS <alias>` namespace (only when reporting).
    Alias,
    /// A `SHOW(…)` field checkbox (index into [`RequestForm::fields`]).
    Field(usize),
}

/// The request configure form ([`Overlay::ReportNodeRequest`]), reached with
/// Enter on a `REQUEST` / `REPORT REQUEST` node: one place to pick the request
/// name, toggle whether it's *reported* (`REPORT`), and — when reported — shape
/// how (its response format `RESPONSE RAW/PRETTY`, its column namespace
/// `AS <alias>`, and which of the fields it can emit are shown via `SHOW(…)`,
/// e.g. to drop a noisy base64 `Response`).
pub(crate) struct RequestForm {
    /// The report being edited (looked up by id, resilient to tab reorder).
    pub(crate) report_id: u64,
    /// Path of the node this edits.
    pub(crate) path: Vec<usize>,
    /// The request name.
    pub(crate) request: String,
    /// Candidate request titles from the bound collection (Name row cycles
    /// through these). Empty when unbound/unresolved.
    pub(crate) titles: Vec<String>,
    /// Whether this is a `REPORT REQUEST` (`true`) or a plain `REQUEST`.
    pub(crate) report: bool,
    /// The `RESPONSE` override: `None` = default (no clause), else RAW/PRETTY.
    pub(crate) response: Option<ResponseFmt>,
    /// The `AS <alias>` namespace; empty = no alias (default = the request name).
    pub(crate) alias: String,
    /// The `SHOW(…)` field checklist.
    pub(crate) fields: Vec<ShowRow>,
    /// The node's `WITH … END` items, preserved verbatim across an edit (the
    /// form doesn't edit them, but must not drop them when re-serializing).
    pub(crate) with: Vec<WithItem>,
    /// Selected row: an index into [`Self::visible_rows`] (clamped on use).
    pub(crate) selected: usize,
}

impl RequestForm {
    /// Build the form for a request node. Field rows are the fields the request
    /// can emit, in canonical output order (intrinsics, then its `[Reports]`
    /// fields, then the node's `WITH` fields), de-duplicated. A field is ticked
    /// when the current `show` is empty (no clause ⇒ all emitted) or names it;
    /// any unknown `show` entry is kept as a ticked row so applying can't
    /// silently drop it.
    #[allow(clippy::too_many_arguments)]
    fn build(
        report_id: u64,
        path: Vec<usize>,
        request: String,
        titles: Vec<String>,
        report: bool,
        alias: Option<String>,
        response: Option<ResponseFmt>,
        current_show: &[String],
        report_fields: &[String],
        with: Vec<WithItem>,
    ) -> Self {
        let with_fields: Vec<String> = with
            .iter()
            .filter_map(|w| match w {
                WithItem::Field { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        let mut names: Vec<String> = Vec::new();
        let push = |name: &str, names: &mut Vec<String>| {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        };
        for f in crate::report::run::INTRINSIC_FIELDS {
            push(f, &mut names);
        }
        for f in report_fields {
            push(f, &mut names);
        }
        for f in &with_fields {
            push(f, &mut names);
        }
        // Preserve any unknown SHOW entry so applying can't drop it.
        for f in current_show {
            push(f, &mut names);
        }
        let all = current_show.is_empty();
        let fields = names
            .into_iter()
            .map(|name| {
                let included = all || current_show.iter().any(|s| s == &name);
                ShowRow { name, included }
            })
            .collect();
        RequestForm {
            report_id,
            path,
            request,
            titles,
            report,
            response,
            alias: alias.unwrap_or_default(),
            fields,
            with,
            selected: 0,
        }
    }

    /// The rows currently on screen, in order. Reporting-only rows (response,
    /// alias, field checklist) appear only when [`Self::report`] is set.
    pub(crate) fn visible_rows(&self) -> Vec<FormRow> {
        let mut rows = vec![FormRow::Name, FormRow::Report];
        if self.report {
            rows.push(FormRow::Response);
            rows.push(FormRow::Alias);
            rows.extend((0..self.fields.len()).map(FormRow::Field));
        }
        rows
    }

    /// The last selectable row index.
    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The `SHOW(…)` field list for the ticked rows, in row order. When every
    /// field is ticked it returns empty (⇒ no `SHOW` clause, the "emit all"
    /// default), so leaving everything on removes any existing clause.
    fn show(&self) -> Vec<String> {
        if self.fields.iter().all(|r| r.included) {
            return Vec::new();
        }
        self.fields
            .iter()
            .filter(|r| r.included)
            .map(|r| r.name.clone())
            .collect()
    }

    /// The `AS <alias>` value, `None` when blank.
    fn alias_opt(&self) -> Option<String> {
        let a = self.alias.trim();
        if a.is_empty() {
            None
        } else {
            Some(a.to_string())
        }
    }

    /// Cycle the request name through the bound collection's titles (a no-op
    /// when there are none). Wraps; starts at the first title when the current
    /// name isn't one of them.
    fn cycle_name(&mut self, forward: bool) {
        let n = self.titles.len();
        if n == 0 {
            return;
        }
        let next = match self.titles.iter().position(|t| t == &self.request) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        self.request = self.titles[next].clone();
    }

    /// Cycle the response-format override: Default → RAW → PRETTY → Default
    /// (reverse when `forward` is false).
    fn cycle_response(&mut self, forward: bool) {
        self.response = if forward {
            match self.response {
                None => Some(ResponseFmt::Raw),
                Some(ResponseFmt::Raw) => Some(ResponseFmt::Pretty),
                Some(ResponseFmt::Pretty) => None,
            }
        } else {
            match self.response {
                None => Some(ResponseFmt::Pretty),
                Some(ResponseFmt::Pretty) => Some(ResponseFmt::Raw),
                Some(ResponseFmt::Raw) => None,
            }
        };
    }
}

// ---------------------------------------------------------------------------
// TuiApp integration
// ---------------------------------------------------------------------------

impl TuiApp {
    pub(crate) fn report_index_by_id(&self, id: u64) -> Option<usize> {
        self.reports.iter().position(|rt| rt.report.id == id)
    }

    /// The flattened node outline for report `idx`, or the parser error message
    /// when its source doesn't currently parse (the node view can't be built
    /// from unparseable text). Request rows are tagged by whether they resolve
    /// in the bound collection.
    pub(crate) fn report_node_rows(&self, idx: usize) -> Result<Vec<NodeRow>, String> {
        let rt = self.reports.get(idx).ok_or("no report")?;
        let flow = rt.report.flow().map_err(|e| e.to_string())?;
        let entries = self
            .resolve_bound_collection(&rt.report)
            .map(|ci| self.collections[ci].entries.as_slice())
            .unwrap_or(&[]);
        let resolves = |name: &str| crate::report::run::resolve_title(entries, name).is_some();
        Ok(flatten(&flow, &resolves))
    }

    /// The bound collection's request titles (for the request picker), empty
    /// when the report isn't bound to a loaded collection.
    fn bound_request_titles(&self, report_id: u64) -> Vec<String> {
        let Some(idx) = self.report_index_by_id(report_id) else {
            return Vec::new();
        };
        self.resolve_bound_collection(&self.reports[idx].report)
            .map(|ci| {
                self.collections[ci]
                    .entries
                    .iter()
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Handle a key in the structured node editor. Returns `true` when the key
    /// was consumed (so the caller stops), `false` to fall through to the
    /// report view's shared shortcuts (global menus, tab nav, the `n` toggle…).
    pub(crate) fn on_key_report_nodes(&mut self, key: KeyEvent, idx: usize) -> bool {
        // Without a parseable flow there are no rows to act on; let the shared
        // shortcuts (e.g. `n`/`e` to drop into the source editor) run instead.
        let Ok(rows) = self.report_node_rows(idx) else {
            return false;
        };
        let last = rows.len().saturating_sub(1);
        let sel = self.reports[idx].node_selected.min(last);
        self.reports[idx].node_selected = sel;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Ctrl+Z reverts the last structural edit (insert/replace/delete/
            // move/folder/detail) — the node editor's undo, mirroring the source
            // editor's in-buffer Ctrl+Z so an accidental change is easy to take
            // back.
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.undo_report_node(idx)
            }
            KeyCode::Up if shift => self.move_selected_node(idx, true),
            KeyCode::Down if shift => self.move_selected_node(idx, false),
            KeyCode::Char('K') => self.move_selected_node(idx, true),
            KeyCode::Char('J') => self.move_selected_node(idx, false),
            KeyCode::Up | KeyCode::Char('k') => {
                self.reports[idx].node_selected = sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.reports[idx].node_selected = (sel + 1).min(last);
            }
            KeyCode::Home => self.reports[idx].node_selected = 0,
            KeyCode::End => self.reports[idx].node_selected = last,
            KeyCode::Char('a') | KeyCode::Insert => self.open_report_node_menu(idx),
            // Enter opens the friendly, structured "configure this node" form
            // (its shape depends on the node kind — request options, a loop's
            // folder, …). `e` is the raw escape hatch that edits the node's
            // source line directly. `f` is deliberately NOT handled here, so it
            // falls through to the shared File menu — consistent with every
            // other view, instead of the old "detail on some kinds, File menu
            // on others" overload.
            KeyCode::Enter => self.configure_selected_node(idx),
            KeyCode::Char('e') => self.edit_selected_node(idx),
            KeyCode::Delete | KeyCode::Backspace => self.delete_selected_node(idx),
            _ => return false,
        }
        true
    }

    /// Open the insert palette for the position implied by the current
    /// selection.
    fn open_report_node_menu(&mut self, idx: usize) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let pos = insert_pos_after(&rows, sel);
        let s = Strings::for_language(&self.language);
        let options = NodeKind::ALL
            .iter()
            .map(|k| k.label(&s).to_string())
            .collect();
        self.overlay = Some(Overlay::ReportNodeMenu(Box::new(NodeMenu {
            step: NodeMenuStep::PickKind,
            options,
            selected: 0,
            pos,
            report_id: self.reports[idx].report.id,
            report_kind: false,
            edit_path: None,
        })));
    }

    /// Open the file browser to choose the source folder for the selected
    /// `FOR … IN FILES/FOLDERS` node. Returns `true` when it applied (the
    /// selection is such a loop), `false` otherwise so the caller falls through
    /// to the shared `f` (File menu) shortcut. The browser reopens at the
    /// loop's current folder when it resolves, else the report's own directory;
    /// the pick is finished on `Space` (see [`Self::commit_report_node_folder`]).
    fn open_report_node_folder(&mut self, idx: usize) -> bool {
        let Ok(rows) = self.report_node_rows(idx) else {
            return false;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else {
            return false;
        };
        let path = row.path.clone();
        let current_dir = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path).and_then(loop_producer_dir) {
                Some(dir) => dir.to_string(),
                None => return false, // not a FILES/FOLDERS loop
            }
        };
        // Reopen the browser at the loop's current folder when it resolves
        // (absolute, or relative to the report), else the report's directory.
        let start = {
            let p = std::path::Path::new(&current_dir);
            if !current_dir.is_empty() && p.is_dir() {
                Some(p.to_path_buf())
            } else if let Some(base) = self.active_report_base_dir() {
                let joined = base.join(&current_dir);
                Some(if joined.is_dir() { joined } else { base })
            } else {
                None
            }
        };
        if let Some(dir) = start {
            self.last_browse_dir = Some(dir);
        }
        self.pending_node_folder = Some((self.reports[idx].report.id, path));
        self.open_browser(crate::tui::app::FileAction::PickReportNodeFolder);
        true
    }

    /// Finish a [`crate::tui::app::FileAction::PickReportNodeFolder`] pick:
    /// write `dir` into the parked loop node's producer, re-serialize,
    /// revalidate and persist. Called from the browser's `Space` handler.
    pub(crate) fn commit_report_node_folder(&mut self, dir: &str) {
        let Some((report_id, path)) = self.pending_node_folder.take() else {
            return;
        };
        let Some(idx) = self.report_index_by_id(report_id) else {
            return;
        };
        {
            let rt = &mut self.reports[idx];
            let Ok(mut flow) = rt.report.flow() else {
                return;
            };
            let Some(node) = node_at_mut(&mut flow, &path) else {
                return;
            };
            match loop_producer_dir_mut(node) {
                Some(slot) => *slot = dir.to_string(),
                None => return,
            }
            let text = flow.to_text();
            rt.set_text_undoable(text);
        }
        self.revalidate_report(idx);
        self.select_node_path(idx, &path);
        self.save_state();
    }

    /// Enter — open the friendly, structured "configure this node" editor for
    /// the selected node. The form depends on the node kind: `Begin` opens the
    /// insert palette; a request node opens the request form (name, `REPORT`
    /// toggle, and — when reported — response/alias/`SHOW`); a `FOR FILES/
    /// FOLDERS` loop opens the folder browser; anything else falls back to the
    /// raw line editor (until it grows its own form). Never touches the File
    /// menu (that's `f`).
    fn configure_selected_node(&mut self, idx: usize) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else { return };
        if row.kind == RowKind::Begin {
            self.open_report_node_menu(idx);
            return;
        }
        let path = row.path.clone();
        // Try the request form, then the loop folder browser; fall back to the
        // raw line editor for kinds without a dedicated form yet.
        if self.open_report_node_request(idx) {
            return;
        }
        if self.open_report_node_folder(idx) {
            return;
        }
        self.open_report_node_line_prompt(idx, &path);
    }

    /// Open the configure form for the selected request node — a plain `REQUEST`
    /// or a `REPORT REQUEST`. Returns `true` when the selection is a request
    /// node, `false` otherwise so the caller can try another form. The `REPORT`
    /// toggle lets a plain request become reported (and back) from here.
    fn open_report_node_request(&mut self, idx: usize) -> bool {
        let Ok(rows) = self.report_node_rows(idx) else {
            return false;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else {
            return false;
        };
        let path = row.path.clone();
        let report_id = self.reports[idx].report.id;
        let (name, report, alias, response, current_show, with) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path) {
                Some(FlowNode::Request { name }) => {
                    (name.clone(), false, None, None, Vec::new(), Vec::new())
                }
                Some(FlowNode::Report(ReportStmt::Request {
                    name,
                    alias,
                    response_fmt,
                    show,
                    with,
                })) => (
                    name.clone(),
                    true,
                    alias.clone(),
                    *response_fmt,
                    show.clone(),
                    with.clone(),
                ),
                _ => return false, // not a request node
            }
        };
        let report_fields = self.request_report_fields(report_id, &name);
        let titles = self.bound_request_titles(report_id);
        let form = RequestForm::build(
            report_id,
            path,
            name,
            titles,
            report,
            alias,
            response,
            &current_show,
            &report_fields,
            with,
        );
        self.overlay = Some(Overlay::ReportNodeRequest(Box::new(form)));
        true
    }

    /// The `[Reports]` field names of the request `name` resolves to in the
    /// report's bound collection, empty when unbound/unresolved.
    fn request_report_fields(&self, report_id: u64, name: &str) -> Vec<String> {
        let Some(idx) = self.report_index_by_id(report_id) else {
            return Vec::new();
        };
        let rt = &self.reports[idx];
        let Some(ci) = self.resolve_bound_collection(&rt.report) else {
            return Vec::new();
        };
        crate::report::run::resolve_title(&self.collections[ci].entries, name)
            .map(|e| e.reports.iter().map(|(f, _)| f.clone()).collect())
            .unwrap_or_default()
    }

    /// Finish a [`RequestForm`]: rebuild the node from the form and write it
    /// back. The `REPORT` toggle chooses the node kind — a plain `REQUEST`
    /// (dropping any reporting options) or a `REPORT REQUEST` carrying the
    /// name, response, alias (blank ⇒ none), `SHOW(…)` (all-ticked ⇒ none) and
    /// the preserved `WITH … END` items. Re-serializes, revalidates, persists.
    pub(crate) fn apply_report_node_request(&mut self, form: RequestForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let node = if form.report {
            FlowNode::Report(ReportStmt::Request {
                name: form.request.clone(),
                alias: form.alias_opt(),
                response_fmt: form.response,
                show: form.show(),
                with: form.with.clone(),
            })
        } else {
            FlowNode::Request {
                name: form.request.clone(),
            }
        };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the request configure form
    /// ([`Overlay::ReportNodeRequest`]). ↑/↓ (or Tab) move between rows; the
    /// name/response rows cycle with Space/←/→; the Report row toggles with
    /// Space; the alias row takes typed identifier characters and Backspace;
    /// field rows toggle with Space/`x`; Enter applies and closes; Esc cancels
    /// (the overlay was already `take`n by the dispatcher).
    pub(crate) fn report_node_request_key_handler(
        &mut self,
        key: KeyEvent,
        mut form: Box<RequestForm>,
    ) {
        let last = form.last_row();
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeRequest(form));
        };
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_request(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                // Resolve which logical row is selected via the dynamic layout,
                // so the reporting-only rows shift correctly when Report is off.
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    // Name — cycle through the bound collection's request titles.
                    Some(FormRow::Name) => match key.code {
                        KeyCode::Char(' ') | KeyCode::Right => {
                            form.cycle_name(true);
                            keep(self, form);
                        }
                        KeyCode::Left => {
                            form.cycle_name(false);
                            keep(self, form);
                        }
                        _ => keep(self, form),
                    },
                    // Report — toggle plain REQUEST ↔ REPORT REQUEST.
                    Some(FormRow::Report) => {
                        if matches!(
                            key.code,
                            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                        ) {
                            form.report = !form.report;
                            form.selected = form.selected.min(form.last_row());
                        }
                        keep(self, form);
                    }
                    // Response override.
                    Some(FormRow::Response) => match key.code {
                        KeyCode::Char(' ') | KeyCode::Right => {
                            form.cycle_response(true);
                            keep(self, form);
                        }
                        KeyCode::Left => {
                            form.cycle_response(false);
                            keep(self, form);
                        }
                        _ => keep(self, form),
                    },
                    // Alias text field (identifier characters only).
                    Some(FormRow::Alias) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_alphanumeric() || c == '_' => {
                                form.alias.push(c)
                            }
                            KeyCode::Backspace => {
                                form.alias.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    // A field checkbox.
                    Some(FormRow::Field(fi)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some(row) = form.fields.get_mut(fi)
                        {
                            row.included = !row.included;
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// `e` — edit the selected node's source line directly (the raw escape
    /// hatch). `Begin` opens the insert palette (there's nothing to edit).
    fn edit_selected_node(&mut self, idx: usize) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else { return };
        if row.kind == RowKind::Begin {
            self.open_report_node_menu(idx);
            return;
        }
        let path = row.path.clone();
        self.open_report_node_line_prompt(idx, &path);
    }

    /// Open the single-line "edit as source" prompt for the node at `path`.
    fn open_report_node_line_prompt(&mut self, idx: usize, path: &[usize]) {
        let report_id = self.reports[idx].report.id;
        let Ok(flow) = self.reports[idx].report.flow() else {
            return;
        };
        let Some(node) = node_at(&flow, path) else {
            return;
        };
        let line = node.header_line();
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::ReportNodeLine {
                report_id,
                path: path.to_vec(),
            },
            editor: Editor::new(&line, false),
            title: format!(
                "{}  ({})",
                s.report_node_edit_title, s.report_node_edit_hint
            ),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Key handling for the insert / request-pick palette
    /// ([`Overlay::ReportNodeMenu`]). Up/Down move; Enter selects (advancing to
    /// the request step or committing); Esc/`q` cancels.
    pub(crate) fn report_node_menu_key_handler(&mut self, key: KeyEvent, mut menu: Box<NodeMenu>) {
        let last = menu.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                menu.selected = menu.selected.saturating_sub(1);
                self.overlay = Some(Overlay::ReportNodeMenu(menu));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                menu.selected = (menu.selected + 1).min(last);
                self.overlay = Some(Overlay::ReportNodeMenu(menu));
            }
            KeyCode::Home => {
                menu.selected = 0;
                self.overlay = Some(Overlay::ReportNodeMenu(menu));
            }
            KeyCode::End => {
                menu.selected = last;
                self.overlay = Some(Overlay::ReportNodeMenu(menu));
            }
            KeyCode::Enter => match menu.step {
                NodeMenuStep::PickKind => self.node_menu_pick_kind(*menu),
                NodeMenuStep::PickRequest => self.node_menu_pick_request(*menu),
            },
            // Esc / q / anything else: cancel (overlay stays taken).
            _ => {}
        }
    }

    fn node_menu_pick_kind(&mut self, mut menu: NodeMenu) {
        let Some(&kind) = NodeKind::ALL.get(menu.selected) else {
            return;
        };
        let Some(idx) = self.report_index_by_id(menu.report_id) else {
            return;
        };
        if kind.needs_request() {
            let report_kind = matches!(kind, NodeKind::ReportRequest);
            let titles = self.bound_request_titles(menu.report_id);
            if titles.is_empty() {
                // No bound collection / no requests: insert an empty-name
                // template and let the user type the name in the line prompt.
                let path = self.apply_node_insert(idx, &menu.pos, request_node("", report_kind));
                self.open_report_node_line_prompt(idx, &path);
                return;
            }
            menu.step = NodeMenuStep::PickRequest;
            menu.options = titles;
            menu.selected = 0;
            menu.report_kind = report_kind;
            self.overlay = Some(Overlay::ReportNodeMenu(Box::new(menu)));
        } else if let Some(node) = kind.template() {
            let path = self.apply_node_insert(idx, &menu.pos, node);
            // Templates carry placeholder fields — open the line prompt so the
            // user fills them in immediately.
            self.open_report_node_line_prompt(idx, &path);
        }
    }

    fn node_menu_pick_request(&mut self, menu: NodeMenu) {
        let Some(idx) = self.report_index_by_id(menu.report_id) else {
            return;
        };
        let Some(name) = menu.options.get(menu.selected) else {
            return;
        };
        let node = request_node(name, menu.report_kind);
        match &menu.edit_path {
            Some(path) => self.apply_node_replace(idx, path, node),
            None => {
                self.apply_node_insert(idx, &menu.pos, node);
            }
        }
    }

    /// Insert `node` at `pos`, re-serialize, revalidate, select the new node,
    /// and persist. Returns the inserted node's path.
    fn apply_node_insert(&mut self, idx: usize, pos: &InsertPos, node: FlowNode) -> Vec<usize> {
        let mut path = pos.parent.clone();
        path.push(pos.index);
        {
            let rt = &mut self.reports[idx];
            let Ok(mut flow) = rt.report.flow() else {
                return path;
            };
            insert_node(&mut flow, pos, node);
            let text = flow.to_text();
            rt.set_text_undoable(text);
        }
        self.revalidate_report(idx);
        self.select_node_path(idx, &path);
        self.save_state();
        path
    }

    /// Replace the node at `path`, re-serialize, revalidate, keep it selected,
    /// and persist.
    fn apply_node_replace(&mut self, idx: usize, path: &[usize], node: FlowNode) {
        {
            let rt = &mut self.reports[idx];
            let Ok(mut flow) = rt.report.flow() else {
                return;
            };
            if !replace_node(&mut flow, path, node) {
                return;
            }
            let text = flow.to_text();
            rt.set_text_undoable(text);
        }
        self.revalidate_report(idx);
        self.select_node_path(idx, path);
        self.save_state();
    }

    fn delete_selected_node(&mut self, idx: usize) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else { return };
        if row.kind == RowKind::Begin {
            return; // the root can't be deleted
        }
        let path = row.path.clone();
        {
            let rt = &mut self.reports[idx];
            let Ok(mut flow) = rt.report.flow() else {
                return;
            };
            if !remove_node(&mut flow, &path) {
                return;
            }
            let text = flow.to_text();
            rt.set_text_undoable(text);
        }
        self.revalidate_report(idx);
        // Selection stays at `sel`; the draw pass clamps it to the new length.
        self.save_state();
    }

    fn move_selected_node(&mut self, idx: usize, up: bool) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(sel) else { return };
        if row.kind == RowKind::Begin {
            return;
        }
        let path = row.path.clone();
        let new_path = {
            let rt = &mut self.reports[idx];
            let Ok(mut flow) = rt.report.flow() else {
                return;
            };
            let Some(np) = move_node(&mut flow, &path, up) else {
                return; // at a boundary — nothing to do
            };
            let text = flow.to_text();
            rt.set_text_undoable(text);
            np
        };
        self.revalidate_report(idx);
        self.select_node_path(idx, &new_path);
        self.save_state();
    }

    /// Undo the last structural node edit (Ctrl+Z in the node editor): pop the
    /// most recent snapshot off this report's [`node_undo`](crate::tui::reports::ReportTab::node_undo)
    /// stack and restore its source text and node selection, then revalidate and
    /// persist. Does nothing (with a brief status) when the stack is empty.
    fn undo_report_node(&mut self, idx: usize) {
        let Some(snap) = self.reports[idx].node_undo.pop() else {
            let s = Strings::for_language(&self.language);
            self.status = Some(Status::ReportNodeNothingToUndo(
                s.report_node_undo_empty.to_string(),
            ));
            return;
        };
        {
            let rt = &mut self.reports[idx];
            rt.report.set_text(snap.text);
            rt.node_selected = snap.node_selected;
        }
        self.revalidate_report(idx);
        self.save_state();
        let s = Strings::for_language(&self.language);
        self.status = Some(Status::ReportNodeUndone(s.report_node_undone.to_string()));
    }

    /// Commit an edited node line (from [`PromptKind::ReportNodeLine`]): re-parse
    /// it and swap it into the flow at `path`, keeping a loop's body.
    pub(crate) fn commit_report_node_line(&mut self, report_id: u64, path: &[usize], text: String) {
        let Some(idx) = self.report_index_by_id(report_id) else {
            return;
        };
        let was_loop = self.reports[idx]
            .report
            .flow()
            .ok()
            .and_then(|flow| node_at(&flow, path).map(FlowNode::is_loop))
            .unwrap_or(false);
        match parse_one_node(&text, was_loop) {
            Some(node) => self.apply_node_replace(idx, path, node),
            None => {
                let s = Strings::for_language(&self.language);
                self.status = Some(Status::ReportRunBlocked(
                    s.report_node_line_invalid.to_string(),
                ));
            }
        }
    }

    /// Move the node-view selection onto the row addressing `path` (the head
    /// row of a loop, or the leaf), clamping if it no longer exists.
    fn select_node_path(&mut self, idx: usize, path: &[usize]) {
        let Ok(rows) = self.report_node_rows(idx) else {
            return;
        };
        let target = rows
            .iter()
            .position(|r| r.path == path && r.kind != RowKind::LoopEnd)
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        self.reports[idx].node_selected = target;
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw the node outline for report `idx` (the middle band of the node view,
/// between the binding row and the validation panel). Renders the flattened
/// rows with the selected row highlighted and auto-scrolls to keep it visible;
/// falls back to the parser error when the source doesn't parse.
pub(crate) fn draw_report_nodes(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let focused = !app.report_tabbar_focus && !app.report_tree_focus;
    let title = format!("{} — {}", s.report_nodes_heading, s.report_nodes_hint);
    let block = panel(title, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = match app.report_node_rows(idx) {
        Ok(rows) => rows,
        Err(e) => {
            let lines = vec![
                Line::from(Span::styled(
                    s.report_nodes_parse_error,
                    Style::default().fg(th.err),
                )),
                Line::from(Span::styled(e, Style::default().fg(th.dim))),
            ];
            f.render_widget(Paragraph::new(lines), inner);
            return;
        }
    };

    let sel = app.reports[idx]
        .node_selected
        .min(rows.len().saturating_sub(1));
    app.reports[idx].node_selected = sel;

    let h = inner.height as usize;
    let w = inner.width as usize;
    let first = if sel >= h { sel + 1 - h } else { 0 };
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, row)| render_node_row(row, i == sel, w, s, th))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);

    if rows.len() > h {
        let bar = Rect {
            x: area.x + area.width - 1,
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        draw_scrollbar(f, bar, rows.len(), h, first, th);
    }
}

fn render_node_row(
    row: &NodeRow,
    selected: bool,
    width: usize,
    s: &Strings,
    th: &Theme,
) -> Line<'static> {
    let indent = "  ".repeat(row.depth);
    let (text, base, bold) = match row.kind {
        RowKind::Begin => (s.report_node_begin.to_string(), th.accent, true),
        RowKind::LoopHead => (row.label.clone(), th.accent, false),
        RowKind::LoopEnd => ("END".to_string(), th.accent, false),
        RowKind::Leaf => (row.label.clone(), th.text, false),
    };
    // Request rows recolour by whether the name resolves (green / amber),
    // matching the source view's highlighting.
    let colour = match row.req_ok {
        Some(true) => th.ok,
        Some(false) => th.pending,
        None => base,
    };
    let mut content = format!("{indent}{text}");
    if selected {
        // Pad to the panel width so the highlight fills the whole row.
        let len = content.chars().count();
        if len < width {
            content.extend(std::iter::repeat_n(' ', width - len));
        }
    }
    let mut style = if selected {
        Style::default().fg(th.select_fg).bg(th.select_bg)
    } else {
        Style::default().fg(colour)
    };
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(content, style))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::report::flow::{FlowNode, ReportStmt};
        match parse_one_node("REPORT FILE AS \"Pretty name\"", false) {
            Some(FlowNode::Report(ReportStmt::VarAs { var, name })) => {
                assert_eq!(var, "FILE");
                assert_eq!(name, "Pretty name");
            }
            other => panic!("expected a VarAs node, got {other:?}"),
        }
    }
}
