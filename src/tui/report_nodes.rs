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
use crate::report::flow::{EnvClause, FlowNode, Pattern, Producer, ReportFlow, ReportStmt};
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
                clause: EnvClause::Plain(Vec::new()),
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

// ---------------------------------------------------------------------------
// TuiApp integration
// ---------------------------------------------------------------------------

impl TuiApp {
    fn report_index_by_id(&self, id: u64) -> Option<usize> {
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
            KeyCode::Enter | KeyCode::Char('e') => self.edit_selected_node(idx),
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

    /// Edit the selected node: request nodes reopen the request picker (to
    /// change the name); every other node opens the "edit as line" prompt.
    /// `Begin` opens the insert palette (there's nothing to edit).
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
        self.edit_node_at(idx, &path);
    }

    fn edit_node_at(&mut self, idx: usize, path: &[usize]) {
        let report_id = self.reports[idx].report.id;
        let (is_request, report_kind, current) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return;
            };
            let Some(node) = node_at(&flow, path) else {
                return;
            };
            let report_kind = matches!(node, FlowNode::Report(ReportStmt::Request { .. }));
            (
                node.request_name().is_some(),
                report_kind,
                node.request_name().map(str::to_string),
            )
        };
        if is_request {
            let titles = self.bound_request_titles(report_id);
            if titles.is_empty() {
                // Nothing to pick from — fall back to editing the line.
                self.open_report_node_line_prompt(idx, path);
                return;
            }
            let selected = current
                .as_deref()
                .and_then(|c| titles.iter().position(|t| t == c))
                .unwrap_or(0);
            self.overlay = Some(Overlay::ReportNodeMenu(Box::new(NodeMenu {
                step: NodeMenuStep::PickRequest,
                options: titles,
                selected,
                pos: InsertPos {
                    parent: Vec::new(),
                    index: 0,
                },
                report_id,
                report_kind,
                edit_path: Some(path.to_vec()),
            })));
        } else {
            self.open_report_node_line_prompt(idx, path);
        }
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
            rt.report.set_text(text);
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
            rt.report.set_text(text);
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
            rt.report.set_text(text);
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
            rt.report.set_text(text);
            np
        };
        self.revalidate_report(idx);
        self.select_node_path(idx, &new_path);
        self.save_state();
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
    let focused = !app.report_tabbar_focus;
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
}
