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

use super::app::{MouseHitTarget, MouseLayer, MouseScrollTarget, Overlay, PromptKind, TuiApp};
use super::draw::panel;
use super::editor::Editor;
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::{Status, Strings};
use crate::report::flow::{
    Element, EnvClause, FlowNode, ParallelSpec, Pattern, Producer, ReportStmt, ResponseFmt,
    RoleRef, WithItem,
};
use crate::report::model::StatKind;

// The pure structural-editing core (flatten/insert/remove/move/replace/parse
// of the flow AST, plus the node-kind palette templates) lives in the
// front-end-agnostic `report::edit` module so the GUI's block editor shares
// one implementation. Re-export it under the historical names so this file's
// TUI-specific rendering / key handling / overlays read unchanged.
pub(crate) use crate::report::edit::{
    InsertPos, NodeKind, NodeRow, RowKind, flatten, insert_node, insert_pos_after,
    loop_producer_dir, loop_producer_dir_mut, move_node, node_at, node_at_mut, parse_one_node,
    remove_node, replace_node, request_node,
};

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
    /// A `HIDE(…)` field checkbox (index into [`RequestForm::hide_fields`]).
    Hidden(usize),
    /// One `WITH name: query` field (index into [`RequestForm::with`]).
    /// Activating it opens the [`WithFieldForm`].
    With(usize),
    /// The "add a `WITH` field" row, which opens an empty [`WithFieldForm`].
    AddWith,
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
    /// The `HIDE(…)` checklist, over the same field names as [`Self::fields`].
    /// A ticked row is *hidden*; nothing ticked ⇒ no `HIDE` clause. `SHOW` and
    /// `HIDE` are separate clauses in the grammar, so they get separate lists
    /// rather than one tri-state per field.
    pub(crate) hide_fields: Vec<ShowRow>,
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
        hide: Vec<String>,
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
        // A `HIDE` entry naming something no request offers is kept too, for
        // the same reason: applying must not drop what the user wrote.
        for f in &hide {
            push(f, &mut names);
        }
        // No SHOW clause means "everything this request already emits" — which
        // excludes the opt-in timing intrinsics, so they must start un-ticked or
        // simply opening and applying the form would switch them on.
        let all = current_show.is_empty();
        let fields: Vec<ShowRow> = names
            .iter()
            .map(|name| {
                let included = (all
                    && !crate::report::run::OPT_IN_INTRINSIC_FIELDS.contains(&name.as_str()))
                    || current_show.iter().any(|s| s == name);
                ShowRow {
                    name: name.clone(),
                    included,
                }
            })
            .collect();
        let hide_fields = names
            .iter()
            .map(|name| ShowRow {
                name: name.clone(),
                included: hide.iter().any(|h| h == name),
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
            hide_fields,
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
            rows.extend((0..self.hide_fields.len()).map(FormRow::Hidden));
            rows.extend((0..self.with.len()).map(FormRow::With));
            rows.push(FormRow::AddWith);
        }
        rows
    }

    /// The `HIDE(…)` list for the ticked rows, in row order. Nothing ticked ⇒
    /// no clause (unlike `SHOW`, where *everything* ticked is the no-clause
    /// case — `HIDE` hides only what it names).
    fn hide(&self) -> Vec<String> {
        self.hide_fields
            .iter()
            .filter(|r| r.included)
            .map(|r| r.name.clone())
            .collect()
    }

    /// The last selectable row index.
    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The `SHOW(…)` field list for the ticked rows, in row order. When the
    /// ticked set is exactly what the request emits with no clause — every
    /// field except the opt-in timing intrinsics — it returns empty (⇒ no
    /// `SHOW` clause), so leaving the form as it opened removes any existing
    /// clause rather than freezing the current selection into one.
    fn show(&self) -> Vec<String> {
        if self.fields.iter().all(|r| {
            r.included != crate::report::run::OPT_IN_INTRINSIC_FIELDS.contains(&r.name.as_str())
        }) {
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

/// One row of the [`VarsForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VarsRow {
    /// One in-scope variable checkbox (index into [`VarsForm::vars`]).
    Var(usize),
    /// The free-text row for a variable the static scan can't see (a value
    /// that only exists at run time, or one supplied by the environment).
    Other,
    /// The `AS <name>` column name — only offered when exactly one variable is
    /// picked, since `REPORT (A, B)` has no single column to name.
    Alias,
    /// One `STATISTICS(…)` checkbox — likewise single-variable only.
    Stat(usize),
}

/// The `REPORT <var>` configure form ([`Overlay::ReportNodeVars`]): which
/// variables become columns, and — for a single variable — the `AS <name>`
/// column name and its `STATISTICS(…)`.
///
/// The two grammar forms it writes are `REPORT (A, B)` for several variables
/// and `REPORT A AS name STATISTICS(…)` for one, so the alias and stat rows
/// appear and disappear with the number ticked rather than being written into
/// a shape that can't hold them.
pub(crate) struct VarsForm {
    pub(crate) report_id: u64,
    pub(crate) path: Vec<usize>,
    /// `(name, ticked)` over the variables in scope at this point in the flow,
    /// plus anything the statement already names.
    pub(crate) vars: Vec<ShowRow>,
    /// A variable typed by hand, for the run-time-only names the static scan
    /// can't enumerate (see [`crate::report::edit::vars_in_scope`]).
    pub(crate) other: String,
    pub(crate) alias: String,
    pub(crate) stats: Vec<(StatKind, bool)>,
    pub(crate) selected: usize,
}

impl VarsForm {
    /// Build the form from the statement's current variables and the names in
    /// scope. Anything the statement already names is ticked and kept, even if
    /// it isn't in scope — applying must never drop what the user wrote.
    fn build(
        report_id: u64,
        path: Vec<usize>,
        chosen: &[String],
        alias: Option<String>,
        stats: &[StatKind],
        in_scope: Vec<String>,
    ) -> Self {
        let mut names = in_scope;
        for c in chosen {
            if !names.iter().any(|n| n == c) {
                names.push(c.clone());
            }
        }
        VarsForm {
            report_id,
            path,
            vars: names
                .into_iter()
                .map(|name| {
                    let included = chosen.iter().any(|c| c == &name);
                    ShowRow { name, included }
                })
                .collect(),
            other: String::new(),
            alias: alias.unwrap_or_default(),
            stats: StatKind::CHOOSABLE
                .iter()
                .map(|k| (*k, stats.contains(k)))
                .collect(),
            selected: 0,
        }
    }

    /// The ticked variables, in row order.
    fn chosen(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .vars
            .iter()
            .filter(|r| r.included)
            .map(|r| r.name.clone())
            .collect();
        let other = self.other.trim();
        if !other.is_empty() && !out.iter().any(|n| n == other) {
            out.push(other.to_string());
        }
        out
    }

    pub(crate) fn visible_rows(&self) -> Vec<VarsRow> {
        let mut rows: Vec<VarsRow> = (0..self.vars.len()).map(VarsRow::Var).collect();
        rows.push(VarsRow::Other);
        // `AS` and `STATISTICS` belong to `REPORT <var> AS <name>`, which holds
        // exactly one variable.
        if self.chosen().len() == 1 {
            rows.push(VarsRow::Alias);
            rows.extend((0..self.stats.len()).map(VarsRow::Stat));
        }
        rows
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The node the rows describe, or `None` when nothing is picked (a
    /// `REPORT` with no variables can't be serialized).
    fn node(&self) -> Option<FlowNode> {
        let chosen = self.chosen();
        let (first, rest) = chosen.split_first()?;
        let alias = self.alias.trim();
        let stats: Vec<StatKind> = self
            .stats
            .iter()
            .filter(|(_, on)| *on)
            .map(|(k, _)| *k)
            .collect();
        // A single variable with a name or statistics is the `VarAs` form;
        // anything else is the plain variable list.
        if rest.is_empty() && (!alias.is_empty() || !stats.is_empty()) {
            return Some(FlowNode::Report(ReportStmt::VarAs {
                var: first.clone(),
                // `STATISTICS` needs a column to attach to, so an unnamed one
                // falls back to the variable's own name.
                name: if alias.is_empty() {
                    first.clone()
                } else {
                    alias.to_string()
                },
                stats,
            }));
        }
        Some(FlowNode::Report(ReportStmt::Vars(chosen)))
    }
}

/// One row of the [`ComputedForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComputedRow {
    /// The quoted template the column's value is built from.
    Template,
    /// The `AS <name>` column name.
    Alias,
    /// One `STATISTICS(…)` checkbox.
    Stat(usize),
}

/// The `REPORT "<template>" AS <name>` configure form
/// ([`Overlay::ReportNodeComputed`]): a computed column's template, its name
/// and its `STATISTICS(…)`.
///
/// The template is free text because it interpolates `{{ … }}` references —
/// there is nothing to pick from a list — but the name and the statistics are
/// structured, and both are required for the statement to re-parse, which is
/// exactly why typing the whole line by hand was easy to get wrong.
pub(crate) struct ComputedForm {
    pub(crate) report_id: u64,
    pub(crate) path: Vec<usize>,
    pub(crate) template: String,
    pub(crate) alias: String,
    pub(crate) stats: Vec<(StatKind, bool)>,
    pub(crate) selected: usize,
}

impl ComputedForm {
    pub(crate) fn visible_rows(&self) -> Vec<ComputedRow> {
        let mut rows = vec![ComputedRow::Template, ComputedRow::Alias];
        rows.extend((0..self.stats.len()).map(ComputedRow::Stat));
        rows
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The node the rows describe. `None` when either half is blank: an empty
    /// template or a missing `AS` name won't re-parse, which would kick the
    /// user out of the node editor entirely.
    fn node(&self) -> Option<FlowNode> {
        let template = self.template.trim();
        let alias = self.alias.trim();
        if template.is_empty() || alias.is_empty() {
            return None;
        }
        Some(FlowNode::Report(ReportStmt::Computed {
            template: template.to_string(),
            name: alias.to_string(),
            stats: self
                .stats
                .iter()
                .filter(|(_, on)| *on)
                .map(|(k, _)| *k)
                .collect(),
        }))
    }
}

/// One row of the [`AssignForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignRow {
    /// The variable name (`VAR = …`).
    Key,
    /// The value it is set to.
    Value,
}

/// The `VARIABLE = VALUE` configure form ([`Overlay::ReportNodeAssign`]): two
/// free-text rows. It exists so a `SET` line doesn't have to be typed as raw
/// source just to change the value it assigns.
pub(crate) struct AssignForm {
    pub(crate) report_id: u64,
    pub(crate) path: Vec<usize>,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) selected: usize,
}

impl AssignForm {
    pub(crate) fn visible_rows(&self) -> Vec<AssignRow> {
        vec![AssignRow::Key, AssignRow::Value]
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The node the rows describe, or `None` when the variable is unnamed (an
    /// assignment with no left-hand side can't be serialized).
    fn node(&self) -> Option<FlowNode> {
        let key = self.key.trim();
        (!key.is_empty()).then(|| FlowNode::Assign {
            key: key.to_string(),
            value: self.value.trim().to_string(),
        })
    }
}

/// One row of the [`ListForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListRow {
    /// The list's name (`LIST NAME = [ … ]`).
    Name,
    /// One scalar element (index into [`ListForm::values`]).
    Value(usize),
    /// The "add an element" row.
    Add,
}

/// The `LIST NAME = [ … ]` configure form ([`Overlay::ReportNodeList`]): the
/// list's name and one row per element.
///
/// Only a *literal* list of scalars is edited here — a tuple list or a computed
/// producer (`ZIP`, `CONCAT`, `TUPLES FROM`) has structure this flat form would
/// flatten away, so those fall through to the raw line editor instead.
pub(crate) struct ListForm {
    pub(crate) report_id: u64,
    pub(crate) path: Vec<usize>,
    pub(crate) name: String,
    pub(crate) values: Vec<String>,
    pub(crate) selected: usize,
}

impl ListForm {
    pub(crate) fn visible_rows(&self) -> Vec<ListRow> {
        let mut rows = vec![ListRow::Name];
        rows.extend((0..self.values.len()).map(ListRow::Value));
        rows.push(ListRow::Add);
        rows
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The node the rows describe, or `None` when the list is unnamed. Blank
    /// element rows are dropped, so deleting an element is just clearing it.
    fn node(&self) -> Option<FlowNode> {
        let name = self.name.trim();
        (!name.is_empty()).then(|| FlowNode::ListDecl {
            name: name.to_string(),
            producer: Producer::List(
                self.values
                    .iter()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .map(|v| Element::Scalar(v.to_string()))
                    .collect(),
            ),
        })
    }
}

/// One row of the [`WithFieldForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithFieldRow {
    /// The column name (`WITH name: …`), editable inline.
    Name,
    /// The Hurl query the column's value comes from, editable inline.
    Query,
    /// One `STATISTICS(…)` checkbox (index into [`StatKind::CHOOSABLE`]).
    Stat(usize),
}

/// The `WITH name: query` field form ([`Overlay::ReportNodeWithField`]): one
/// ad-hoc column of a report request's `WITH … END` block.
///
/// It edits a single field rather than the whole block because a `WITH` item is
/// two free-text values plus a checklist — far more than a row of the request
/// form can carry — and because adding one field at a time is how the block is
/// actually built up.
pub(crate) struct WithFieldForm {
    /// The report being edited (looked up by id, resilient to tab reorder).
    pub(crate) report_id: u64,
    /// Path of the report-request node whose `WITH` block this edits.
    pub(crate) path: Vec<usize>,
    /// The index being edited, or `None` to append a new field.
    pub(crate) index: Option<usize>,
    pub(crate) name: String,
    pub(crate) query: String,
    /// `(stat, ticked)` over [`StatKind::CHOOSABLE`], in that order. None
    /// ticked ⇒ no `STATISTICS(…)` clause.
    pub(crate) stats: Vec<(StatKind, bool)>,
    /// Selected row: an index into [`Self::visible_rows`] (clamped on use).
    pub(crate) selected: usize,
}

impl WithFieldForm {
    fn build(
        report_id: u64,
        path: Vec<usize>,
        index: Option<usize>,
        existing: Option<&WithItem>,
    ) -> Self {
        let (name, query, stats) = match existing {
            Some(WithItem::Field { name, query, stats }) => {
                (name.clone(), query.clone(), stats.clone())
            }
            // A bare `WITH RESPONSE` isn't a named field, so editing it falls
            // through to a fresh one rather than silently rewriting it.
            _ => (String::new(), String::new(), Vec::new()),
        };
        WithFieldForm {
            report_id,
            path,
            index,
            name,
            query,
            stats: StatKind::CHOOSABLE
                .iter()
                .map(|k| (*k, stats.contains(k)))
                .collect(),
            selected: 0,
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<WithFieldRow> {
        let mut rows = vec![WithFieldRow::Name, WithFieldRow::Query];
        rows.extend((0..self.stats.len()).map(WithFieldRow::Stat));
        rows
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// The field the rows describe, or `None` when it has no name (an unnamed
    /// column can't be written, so the caller leaves the block unchanged).
    fn item(&self) -> Option<WithItem> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        Some(WithItem::Field {
            name: name.to_string(),
            query: self.query.trim().to_string(),
            stats: self
                .stats
                .iter()
                .filter(|(_, on)| *on)
                .map(|(k, _)| *k)
                .collect(),
        })
    }
}

/// One row of the [`EnvsForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvsRow {
    /// The loop variable name (a free identifier, editable inline).
    Var,
    /// The Iterate (`Plain`) vs Compare (`Roles`) mode toggle.
    Mode,
    /// The `PARALLEL` on/off toggle (run iterations concurrently).
    Parallel,
    /// `PARALLEL(n)`'s max-concurrency, typed as digits. Only shown while
    /// `PARALLEL` is on, since a degree without the marker means nothing.
    Degree,
    /// One environment entry (index into [`EnvsForm::entries`]).
    Env(usize),
    /// One `BASELINE(…) SHOW(…)` field checkbox (index into
    /// [`EnvsForm::baseline_show`]). Compare mode only.
    BaselineShow(usize),
}

/// One chosen environment in the [`EnvsForm`]. `baseline` is only meaningful in
/// Compare mode (at most one entry is the baseline; the rest are comparisons).
/// `file` marks a `FILE("…")` snapshot reference (a saved baseline reused in
/// place of a live run) rather than a loaded environment name.
pub(crate) struct EnvEntry {
    pub(crate) name: String,
    pub(crate) baseline: bool,
    pub(crate) file: bool,
}

/// The `FOR … IN ENVS` configure form ([`Overlay::ReportNodeEnvs`]), reached
/// with Enter on an `ENVS` loop node. It picks the loop variable, the mode
/// (Iterate = `ENVS "a", "b"` vs Compare = `ENVS BASELINE(…), COMPARISON(…)`)
/// and — the point of #11 — the environment names from the *loaded*
/// environments rather than typing them by hand.
pub(crate) struct EnvsForm {
    /// The report being edited (looked up by id, resilient to tab reorder).
    pub(crate) report_id: u64,
    /// Path of the node this edits.
    pub(crate) path: Vec<usize>,
    /// The loop variable name.
    pub(crate) var: String,
    /// `false` = Iterate (`Plain`), `true` = Compare (`Roles`).
    pub(crate) compare: bool,
    /// `true` when the loop is marked `PARALLEL` (iterations run concurrently).
    pub(crate) parallel: bool,
    /// The chosen environments, in row order.
    pub(crate) entries: Vec<EnvEntry>,
    /// Loaded environment names the env rows cycle through (empty ⇒ no picker).
    pub(crate) choices: Vec<String>,
    /// Discovered `.baseline` snapshot paths (relative to the report root) that a
    /// `FILE(…)` role entry cycles through — the file analogue of [`Self::choices`].
    /// Seeded from the report directory plus any snapshot paths already in the
    /// clause, so an existing `FILE(…)` value is always in the cycle.
    pub(crate) snapshots: Vec<String>,
    /// Selected row: an index into [`Self::visible_rows`] (clamped on use).
    pub(crate) selected: usize,
    /// `PARALLEL(n)`'s max-concurrency as typed text, so the row can be left
    /// blank (meaning "use the prelude's `MAX_PARALLEL`") and half-typed input
    /// isn't clamped under the cursor.
    pub(crate) degree: String,
    /// `BASELINE(…) SHOW(…)` as a checklist over every field the loop body's
    /// reported requests can emit. Nothing ticked means *no* `SHOW` clause,
    /// which for a baseline is "carry nothing across" — the opposite of a
    /// request's `SHOW`, where empty means "emit everything". So nothing is
    /// ticked by default.
    pub(crate) baseline_show: Vec<ShowRow>,
}

impl EnvsForm {
    /// Build the form from a node's current variable and [`EnvClause`].
    /// `choices` are the loaded environment names an env entry cycles through;
    /// `snapshots` are the discovered `.baseline` paths a `FILE(…)` entry cycles.
    #[allow(clippy::too_many_arguments)]
    fn build(
        report_id: u64,
        path: Vec<usize>,
        var: String,
        clause: &EnvClause,
        parallel: Option<ParallelSpec>,
        choices: Vec<String>,
        mut snapshots: Vec<String>,
        show_choices: Vec<(String, bool)>,
    ) -> Self {
        let (compare, mut entries, baseline_show_names) = match clause {
            EnvClause::Plain(names) => (
                false,
                names
                    .iter()
                    .map(|n| EnvEntry {
                        name: n.clone(),
                        baseline: false,
                        file: false,
                    })
                    .collect::<Vec<_>>(),
                Vec::new(),
            ),
            EnvClause::Roles {
                baseline,
                comparisons,
                baseline_show,
            } => {
                let entry = |r: &RoleRef, is_baseline: bool| EnvEntry {
                    name: r.target().to_string(),
                    baseline: is_baseline,
                    file: matches!(r, RoleRef::File(_)),
                };
                let mut es: Vec<EnvEntry> = baseline.iter().map(|r| entry(r, true)).collect();
                es.extend(comparisons.iter().map(|r| entry(r, false)));
                (true, es, baseline_show.clone())
            }
        };
        // Ensure any snapshot path already used by a FILE entry is in the cycle,
        // even if it no longer exists on disk (so an existing value survives and
        // is reachable by cycling).
        for e in &entries {
            if e.file && !e.name.trim().is_empty() && !snapshots.iter().any(|s| s == &e.name) {
                snapshots.push(e.name.clone());
            }
        }
        // The clause always keeps at least one entry so it can't serialize to an
        // empty (unparseable) `FOR VAR IN ENVS `.
        if entries.is_empty() {
            entries.push(EnvEntry {
                name: choices.first().cloned().unwrap_or_default(),
                baseline: compare,
                file: false,
            });
        }
        // The checklist is built by the caller (it needs the loop body and the
        // bound collection); anything the clause already names but the body no
        // longer offers is appended there, so applying can't silently drop a
        // field the user wrote by hand.
        let mut baseline_show: Vec<ShowRow> = show_choices
            .into_iter()
            .map(|(name, included)| ShowRow { name, included })
            .collect();
        for name in &baseline_show_names {
            if !baseline_show.iter().any(|r| &r.name == name) {
                baseline_show.push(ShowRow {
                    name: name.clone(),
                    included: true,
                });
            }
        }
        EnvsForm {
            report_id,
            path,
            var,
            compare,
            parallel: parallel.is_some(),
            degree: parallel
                .and_then(|p| p.degree)
                .map(|d| d.to_string())
                .unwrap_or_default(),
            entries,
            choices,
            snapshots,
            selected: 0,
            baseline_show,
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<EnvsRow> {
        let mut rows = vec![EnvsRow::Var, EnvsRow::Mode, EnvsRow::Parallel];
        if self.parallel {
            rows.push(EnvsRow::Degree);
        }
        rows.extend((0..self.entries.len()).map(EnvsRow::Env));
        // `SHOW` selects what the baseline carries across into the comparison,
        // so it only exists in Compare mode — and only once there is a baseline
        // for it to qualify.
        if self.compare && self.entries.iter().any(|e| e.baseline) {
            rows.extend((0..self.baseline_show.len()).map(EnvsRow::BaselineShow));
        }
        rows
    }

    /// The `PARALLEL` spec the rows describe: `None` when the toggle is off,
    /// else the typed degree (a blank or unparseable box means "no explicit
    /// limit", i.e. fall back to the prelude's `MAX_PARALLEL`).
    fn parallel_spec(&self) -> Option<ParallelSpec> {
        self.parallel.then(|| ParallelSpec {
            degree: self.degree.trim().parse::<u32>().ok().filter(|d| *d > 0),
        })
    }

    /// The ticked `BASELINE(…) SHOW(…)` fields, in checklist order.
    fn selected_baseline_show(&self) -> Vec<String> {
        self.baseline_show
            .iter()
            .filter(|r| r.included)
            .map(|r| r.name.clone())
            .collect()
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    /// Cycle one entry's value through the loaded environment names (or, for a
    /// `FILE(…)` entry, the discovered snapshot paths) — a no-op when the
    /// relevant list is empty, so a fresh template's placeholders survive.
    fn cycle_entry(&mut self, i: usize, forward: bool) {
        let list = if self.entries[i].file {
            &self.snapshots
        } else {
            &self.choices
        };
        let n = list.len();
        if n == 0 {
            return;
        }
        let cur = &self.entries[i].name;
        let next = match list.iter().position(|c| c == cur) {
            Some(p) if forward => (p + 1) % n,
            Some(p) => (p + n - 1) % n,
            None => 0,
        };
        self.entries[i].name = list[next].clone();
    }

    /// Toggle whether entry `i` is a `FILE(…)` snapshot reference (Compare mode
    /// only — a plain `ENVS` list can't hold snapshots). Switching sets the
    /// entry's value to the first item of the newly-relevant list so it starts
    /// valid, unless it already matches one.
    fn toggle_file(&mut self, i: usize) {
        if !self.compare {
            return;
        }
        let becoming_file = !self.entries[i].file;
        self.entries[i].file = becoming_file;
        let list = if becoming_file {
            &self.snapshots
        } else {
            &self.choices
        };
        if !list.iter().any(|c| c == &self.entries[i].name)
            && let Some(first) = list.first()
        {
            self.entries[i].name = first.clone();
        }
    }

    /// Toggle whether entry `i` is the baseline (Compare mode only). Enforces
    /// the "at most one baseline" rule by clearing every other entry's flag.
    fn toggle_baseline(&mut self, i: usize) {
        if !self.compare {
            return;
        }
        let becoming = !self.entries[i].baseline;
        for (j, e) in self.entries.iter_mut().enumerate() {
            e.baseline = becoming && j == i;
        }
    }

    /// Flip Iterate ↔ Compare. Entering Compare with no baseline promotes the
    /// first entry so a comparison run has a reference by default.
    fn toggle_mode(&mut self) {
        self.compare = !self.compare;
        if self.compare
            && !self.entries.iter().any(|e| e.baseline)
            && let Some(first) = self.entries.first_mut()
        {
            first.baseline = true;
        }
    }

    /// Flip the `PARALLEL` marker on/off.
    fn toggle_parallel(&mut self) {
        self.parallel = !self.parallel;
    }

    fn add_entry(&mut self) {
        self.entries.push(EnvEntry {
            name: self.choices.first().cloned().unwrap_or_default(),
            baseline: false,
            file: false,
        });
    }

    fn remove_entry(&mut self, i: usize) {
        if self.entries.len() > 1 && i < self.entries.len() {
            self.entries.remove(i);
        }
    }

    fn var_or_default(&self) -> String {
        let v = self.var.trim();
        if v.is_empty() {
            "TARGET".to_string()
        } else {
            v.to_string()
        }
    }

    /// The [`EnvClause`] the current rows describe, or `None` when it would be
    /// empty (nothing named) — the caller then leaves the node unchanged rather
    /// than writing an unparseable clause.
    fn clause(&self) -> Option<EnvClause> {
        if self.compare {
            let refs = |want_baseline: bool| -> Vec<RoleRef> {
                self.entries
                    .iter()
                    .filter(|e| e.baseline == want_baseline)
                    .filter(|e| !e.name.trim().is_empty())
                    .map(|e| {
                        let name = e.name.trim().to_string();
                        if e.file {
                            RoleRef::File(name)
                        } else {
                            RoleRef::Env(name)
                        }
                    })
                    .collect()
            };
            let baseline = refs(true);
            let comparisons = refs(false);
            if baseline.is_empty() && comparisons.is_empty() {
                return None;
            }
            Some(EnvClause::Roles {
                baseline,
                comparisons,
                baseline_show: self.selected_baseline_show(),
            })
        } else {
            let names: Vec<String> = self
                .entries
                .iter()
                .map(|e| e.name.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            if names.is_empty() {
                return None;
            }
            Some(EnvClause::Plain(names))
        }
    }
}

/// One row of the [`FilesForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesRow {
    /// The loop variable name (a free identifier, editable inline).
    Var,
    /// The source folder — activating it opens the file picker.
    Folder,
    /// The optional `MATCH "glob"` filter (editable text; empty ⇒ no `MATCH`).
    Match,
    /// The `PARALLEL` on/off toggle (run iterations concurrently).
    Parallel,
    /// `PARALLEL(n)`'s max-concurrency, typed as digits. Only shown while
    /// `PARALLEL` is on.
    Degree,
}

/// The `FOR … IN FILES` / `FOR … IN FOLDERS` configure form
/// ([`Overlay::ReportNodeFiles`]), reached with Enter on either loop — the file
/// analogue of [`EnvsForm`]. It picks the loop variable, the source folder (via
/// the file picker), an optional `MATCH` glob (`FILES` only) and whether the
/// loop runs `PARALLEL`, with an optional max-concurrency.
///
/// The two producers share one form because they differ only in that `FOLDERS`
/// has no `MATCH` and instead carries `WITH role="glob"` clauses, which the
/// form preserves verbatim rather than editing.
pub(crate) struct FilesForm {
    /// The report being edited (looked up by id, resilient to tab reorder).
    pub(crate) report_id: u64,
    /// Path of the node this edits.
    pub(crate) path: Vec<usize>,
    /// The loop variable name.
    pub(crate) var: String,
    /// The source directory the loop reads from (as authored — may be relative
    /// to the report). Chosen via the folder picker on the Folder row.
    pub(crate) dir: String,
    /// The `MATCH "glob"` filter (empty ⇒ no `MATCH` clause).
    pub(crate) glob: String,
    /// `true` when the loop is marked `PARALLEL` (iterations run concurrently).
    pub(crate) parallel: bool,
    /// `PARALLEL(n)`'s max-concurrency as typed text; blank ⇒ no explicit limit.
    pub(crate) degree: String,
    /// `true` when this edits a `FOLDERS` loop rather than a `FILES` one.
    pub(crate) folders: bool,
    /// A `FOLDERS` loop's `WITH role="glob"` clauses, preserved verbatim across
    /// an edit (the form doesn't expose them, but must not drop them).
    pub(crate) roles: Vec<(String, String)>,
    /// Selected row: an index into [`Self::visible_rows`] (clamped on use).
    pub(crate) selected: usize,
}

impl FilesForm {
    /// Build the form from a `FILES` loop's current variable, directory, glob
    /// and parallel marker. A freshly-inserted loop (empty `dir`) starts with
    /// the Folder row selected so the picker is one keystroke away — the source
    /// directory is the whole point of the loop.
    #[allow(clippy::too_many_arguments)]
    fn build(
        report_id: u64,
        path: Vec<usize>,
        var: String,
        dir: String,
        glob: Option<String>,
        parallel: Option<ParallelSpec>,
        folders: bool,
        roles: Vec<(String, String)>,
    ) -> Self {
        let selected = if dir.trim().is_empty() { 1 } else { 0 };
        FilesForm {
            report_id,
            path,
            var,
            dir,
            glob: glob.unwrap_or_default(),
            parallel: parallel.is_some(),
            degree: parallel
                .and_then(|p| p.degree)
                .map(|d| d.to_string())
                .unwrap_or_default(),
            folders,
            roles,
            selected,
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<FilesRow> {
        let mut rows = vec![FilesRow::Var, FilesRow::Folder];
        // `FOLDERS` has no `MATCH` clause in the grammar, so the row would be
        // a field that can't be written.
        if !self.folders {
            rows.push(FilesRow::Match);
        }
        rows.push(FilesRow::Parallel);
        if self.parallel {
            rows.push(FilesRow::Degree);
        }
        rows
    }

    /// The `PARALLEL` spec the rows describe (see [`EnvsForm::parallel_spec`]).
    fn parallel_spec(&self) -> Option<ParallelSpec> {
        self.parallel.then(|| ParallelSpec {
            degree: self.degree.trim().parse::<u32>().ok().filter(|d| *d > 0),
        })
    }

    /// The producer the rows describe.
    fn producer(&self) -> Producer {
        if self.folders {
            Producer::Folders {
                dir: self.dir.clone(),
                roles: self.roles.clone(),
            }
        } else {
            Producer::Files {
                dir: self.dir.clone(),
                glob: self.glob_opt(),
            }
        }
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }

    fn var_or_default(&self) -> String {
        let v = self.var.trim();
        if v.is_empty() {
            "FILE".to_string()
        } else {
            v.to_string()
        }
    }

    /// The `MATCH` glob as an `Option` (trimmed; empty ⇒ `None`).
    fn glob_opt(&self) -> Option<String> {
        let g = self.glob.trim();
        if g.is_empty() {
            None
        } else {
            Some(g.to_string())
        }
    }

    fn toggle_parallel(&mut self) {
        self.parallel = !self.parallel;
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
    /// FOLDERS` loop opens the folder browser; reported variables and computed
    /// columns open their own forms. Only the kinds with no palette entry
    /// (tuple-pattern loops, tuple list literals, exotic producers) fall back to
    /// the raw line editor. Never touches the File menu (that's `f`).
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
        if self.open_report_node_envs(idx) {
            return;
        }
        if self.open_report_node_files(idx) {
            return;
        }
        if self.open_report_node_vars(idx) {
            return;
        }
        if self.open_report_node_computed(idx) {
            return;
        }
        if self.open_report_node_assign(idx) {
            return;
        }
        if self.open_report_node_list(idx) {
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
    /// Build a [`RequestForm`] for `node` at `path` in report `idx`, or `None`
    /// when the node isn't a request. Shared by the "open it" gesture and by
    /// the `WITH` sub-form's return path, so both land on an identically
    /// populated form.
    fn build_report_node_request_form(
        &self,
        idx: usize,
        path: Vec<usize>,
        node: &FlowNode,
    ) -> Option<RequestForm> {
        let report_id = self.reports[idx].report.id;
        let (name, report, alias, response, current_show, current_hide, with) = match node {
            FlowNode::Request { name } => (
                name.clone(),
                false,
                None,
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            FlowNode::Report(ReportStmt::Request {
                name,
                alias,
                response_fmt,
                show,
                hide,
                with,
            }) => (
                name.clone(),
                true,
                alias.clone(),
                *response_fmt,
                show.clone(),
                hide.clone(),
                with.clone(),
            ),
            _ => return None,
        };
        let report_fields = self.request_report_fields(report_id, &name);
        let titles = self.bound_request_titles(report_id);
        Some(RequestForm::build(
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
            current_hide,
        ))
    }

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
        let Ok(flow) = self.reports[idx].report.flow() else {
            return false;
        };
        let Some(node) = node_at(&flow, &path).cloned() else {
            return false;
        };
        let Some(form) = self.build_report_node_request_form(idx, path, &node) else {
            return false; // not a request node
        };
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
    /// name, response, alias (blank ⇒ none), `SHOW(…)` (all-ticked ⇒ none), the
    /// preserved `HIDE(…)` clause, and the preserved `WITH … END` items.
    /// Re-serializes, revalidates, persists.
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
                hide: form.hide(),
                with: form.with.clone(),
            })
        } else {
            FlowNode::Request {
                name: form.request.clone(),
            }
        };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Open the `REPORT <var>` form for the selected node. Returns `true` when
    /// the selection is a reported-variable statement.
    fn open_report_node_vars(&mut self, idx: usize) -> bool {
        let Some((report_id, path, node)) = self.selected_node(idx) else {
            return false;
        };
        let (chosen, alias, stats) = match &node {
            FlowNode::Report(ReportStmt::Vars(vars)) => (vars.clone(), None, Vec::new()),
            FlowNode::Report(ReportStmt::VarAs { var, name, stats }) => {
                (vec![var.clone()], Some(name.clone()), stats.clone())
            }
            _ => return false,
        };
        // The candidate list needs the bound collection to include the captures
        // of requests already sent; without one it is just the flow's own
        // assignments and loop binders.
        let entries = self
            .resolve_bound_collection(&self.reports[idx].report)
            .map(|ci| self.collections[ci].entries.clone())
            .unwrap_or_default();
        let in_scope = match self.reports[idx].report.flow() {
            Ok(flow) => crate::report::edit::vars_in_scope(&flow, &path, &entries),
            Err(_) => Vec::new(),
        };
        self.overlay = Some(Overlay::ReportNodeVars(Box::new(VarsForm::build(
            report_id, path, &chosen, alias, &stats, in_scope,
        ))));
        true
    }

    /// Write a [`VarsForm`] back. Picking nothing is a no-op.
    pub(crate) fn apply_report_node_vars(&mut self, form: VarsForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(node) = form.node() else { return };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the `REPORT <var>` form. Variable and stat rows toggle
    /// with Space/`x`; the free-text and alias rows take typed characters.
    pub(crate) fn report_node_vars_key_handler(&mut self, key: KeyEvent, mut form: Box<VarsForm>) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeVars(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_vars(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    Some(VarsRow::Var(vi)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some(row) = form.vars.get_mut(vi)
                        {
                            row.included = !row.included;
                            // Ticking a second variable hides the alias/stat
                            // rows, which can leave the selection past the end.
                            form.selected = form.selected.min(form.last_row());
                        }
                        keep(self, form);
                    }
                    Some(VarsRow::Other) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_alphanumeric() || c == '_' => {
                                form.other.push(c)
                            }
                            KeyCode::Backspace => {
                                form.other.pop();
                            }
                            _ => {}
                        }
                        form.selected = form.selected.min(form.last_row());
                        keep(self, form);
                    }
                    Some(VarsRow::Alias) => {
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
                    Some(VarsRow::Stat(si)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some((_, on)) = form.stats.get_mut(si)
                        {
                            *on = !*on;
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// Open the `REPORT "<template>" AS <name>` form for the selected node.
    /// Returns `true` when the selection is a computed column.
    fn open_report_node_computed(&mut self, idx: usize) -> bool {
        let Some((report_id, path, node)) = self.selected_node(idx) else {
            return false;
        };
        let FlowNode::Report(ReportStmt::Computed {
            template,
            name,
            stats,
        }) = node
        else {
            return false;
        };
        self.overlay = Some(Overlay::ReportNodeComputed(Box::new(ComputedForm {
            report_id,
            path,
            template,
            alias: name,
            stats: StatKind::CHOOSABLE
                .iter()
                .map(|k| (*k, stats.contains(k)))
                .collect(),
            selected: 0,
        })));
        true
    }

    /// Write a [`ComputedForm`] back. A blank template or name is a no-op.
    pub(crate) fn apply_report_node_computed(&mut self, form: ComputedForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(node) = form.node() else { return };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the computed-column form. The template takes any
    /// printable character (it interpolates `{{ … }}`); the name is an
    /// identifier; stat rows toggle with Space/`x`.
    pub(crate) fn report_node_computed_key_handler(
        &mut self,
        key: KeyEvent,
        mut form: Box<ComputedForm>,
    ) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeComputed(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_computed(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    Some(ComputedRow::Template) => {
                        match key.code {
                            KeyCode::Char(c) => form.template.push(c),
                            KeyCode::Backspace => {
                                form.template.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(ComputedRow::Alias) => {
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
                    Some(ComputedRow::Stat(si)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some((_, on)) = form.stats.get_mut(si)
                        {
                            *on = !*on;
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// Open the `VARIABLE = VALUE` form for the selected node. Returns `true`
    /// when the selection is an assignment (so the caller stops trying other
    /// forms).
    fn open_report_node_assign(&mut self, idx: usize) -> bool {
        let Some((report_id, path, node)) = self.selected_node(idx) else {
            return false;
        };
        let FlowNode::Assign { key, value } = node else {
            return false;
        };
        self.overlay = Some(Overlay::ReportNodeAssign(Box::new(AssignForm {
            report_id,
            path,
            key,
            value,
            selected: 0,
        })));
        true
    }

    /// Write an [`AssignForm`] back. A blank variable name is a no-op.
    pub(crate) fn apply_report_node_assign(&mut self, form: AssignForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(node) = form.node() else { return };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the `VARIABLE = VALUE` form. Both rows are free text
    /// (a value may be a `{{ … }}` reference or anything else), so they take
    /// any printable character.
    pub(crate) fn report_node_assign_key_handler(
        &mut self,
        key: KeyEvent,
        mut form: Box<AssignForm>,
    ) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeAssign(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_assign(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                let target = match rows.get(sel).copied() {
                    Some(AssignRow::Key) => &mut form.key,
                    Some(AssignRow::Value) => &mut form.value,
                    None => {
                        keep(self, form);
                        return;
                    }
                };
                match key.code {
                    KeyCode::Char(c) => target.push(c),
                    KeyCode::Backspace => {
                        target.pop();
                    }
                    _ => {}
                }
                keep(self, form);
            }
        }
    }

    /// Open the `LIST NAME = [ … ]` form for the selected node. Returns `true`
    /// only for a *literal* list of scalars — a tuple list or a computed
    /// producer falls through to the raw editor, which can express it.
    fn open_report_node_list(&mut self, idx: usize) -> bool {
        let Some((report_id, path, node)) = self.selected_node(idx) else {
            return false;
        };
        let FlowNode::ListDecl {
            name,
            producer: Producer::List(elems),
        } = node
        else {
            return false;
        };
        let mut values = Vec::with_capacity(elems.len());
        for e in &elems {
            match e {
                Element::Scalar(v) => values.push(v.clone()),
                Element::Tuple(_) => return false, // structure this form would flatten
            }
        }
        self.overlay = Some(Overlay::ReportNodeList(Box::new(ListForm {
            report_id,
            path,
            name,
            values,
            selected: 0,
        })));
        true
    }

    /// Write a [`ListForm`] back. A blank list name is a no-op.
    pub(crate) fn apply_report_node_list(&mut self, form: ListForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(node) = form.node() else { return };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the `LIST` form. Name and element rows take any
    /// printable character; the Add row appends an element with Space, and
    /// `x`/Del removes the selected element.
    pub(crate) fn report_node_list_key_handler(&mut self, key: KeyEvent, mut form: Box<ListForm>) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeList(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_list(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    Some(ListRow::Name) => {
                        match key.code {
                            KeyCode::Char(c) => form.name.push(c),
                            KeyCode::Backspace => {
                                form.name.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(ListRow::Value(vi)) => {
                        match key.code {
                            // Del removes the whole element; Backspace edits it,
                            // so a half-typed value isn't lost to a stray key.
                            KeyCode::Delete => {
                                if vi < form.values.len() {
                                    form.values.remove(vi);
                                }
                                form.selected = form.selected.min(form.last_row());
                            }
                            KeyCode::Char(c) => form.values[vi].push(c),
                            KeyCode::Backspace => {
                                form.values[vi].pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(ListRow::Add) => {
                        if matches!(key.code, KeyCode::Char(' ')) {
                            form.values.push(String::new());
                            // Land on the new (empty) row so it can be typed
                            // into straight away.
                            form.selected = form.values.len();
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// The report id, path and a clone of the node the node editor's selection
    /// points at — the common preamble of every `open_report_node_*`.
    fn selected_node(&self, idx: usize) -> Option<(u64, Vec<usize>, FlowNode)> {
        let rows = self.report_node_rows(idx).ok()?;
        let sel = self.reports[idx]
            .node_selected
            .min(rows.len().saturating_sub(1));
        let path = rows.get(sel)?.path.clone();
        let flow = self.reports[idx].report.flow().ok()?;
        let node = node_at(&flow, &path)?.clone();
        Some((self.reports[idx].report.id, path, node))
    }

    /// Key handling for the `WITH` field form ([`Overlay::ReportNodeWithField`]).
    /// ↑/↓ (or Tab) move; the Name/Query rows take typed text; stat rows toggle
    /// with Space/`x`; Enter applies and reopens the request form; Esc cancels
    /// back to it, so the sub-form always returns where it came from.
    pub(crate) fn report_node_with_field_key_handler(
        &mut self,
        key: KeyEvent,
        mut form: Box<WithFieldForm>,
    ) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeWithField(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => {
                let (report_id, path) = (form.report_id, form.path.clone());
                self.apply_report_node_with_field(*form);
                self.reopen_report_node_request(report_id, &path);
            }
            KeyCode::Esc => {
                let (report_id, path) = (form.report_id, form.path.clone());
                self.reopen_report_node_request(report_id, &path);
            }
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    // The column name is an identifier-ish label; the query is
                    // arbitrary Hurl (JSONPath, headers, …), so it takes any
                    // printable character.
                    Some(WithFieldRow::Name) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_alphanumeric() || c == '_' => {
                                form.name.push(c)
                            }
                            KeyCode::Backspace => {
                                form.name.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(WithFieldRow::Query) => {
                        match key.code {
                            KeyCode::Char(c) => form.query.push(c),
                            KeyCode::Backspace => {
                                form.query.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(WithFieldRow::Stat(si)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some((_, on)) = form.stats.get_mut(si)
                        {
                            *on = !*on;
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// Write a [`WithFieldForm`] back into its request node's `WITH … END`
    /// block — replacing the field at `index`, or appending when it is `None`.
    /// A blank name is a no-op (an unnamed column can't be serialized), which
    /// is also how "cancel by clearing the name" behaves.
    pub(crate) fn apply_report_node_with_field(&mut self, form: WithFieldForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(item) = form.item() else {
            return;
        };
        let Ok(flow) = self.reports[idx].report.flow() else {
            return;
        };
        let Some(FlowNode::Report(ReportStmt::Request {
            name,
            alias,
            response_fmt,
            show,
            hide,
            with,
        })) = node_at(&flow, &form.path)
        else {
            return;
        };
        let mut with = with.clone();
        match form.index {
            Some(i) if i < with.len() => with[i] = item,
            _ => with.push(item),
        }
        let node = FlowNode::Report(ReportStmt::Request {
            name: name.clone(),
            alias: alias.clone(),
            response_fmt: *response_fmt,
            show: show.clone(),
            hide: hide.clone(),
            with,
        });
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Reopen the request form for the node at `path` after a `WITH` sub-form
    /// closes, so the user lands back where they were rather than in the node
    /// list. Silently does nothing when the node has gone (the report was
    /// closed or edited underneath).
    fn reopen_report_node_request(&mut self, report_id: u64, path: &[usize]) {
        let Some(idx) = self.report_index_by_id(report_id) else {
            return;
        };
        let Ok(flow) = self.reports[idx].report.flow() else {
            return;
        };
        let Some(node) = node_at(&flow, path).cloned() else {
            return;
        };
        if let Some(form) = self.build_report_node_request_form(idx, path.to_vec(), &node) {
            self.overlay = Some(Overlay::ReportNodeRequest(Box::new(form)));
        }
    }

    /// Open the configure form for the selected `FOR … IN ENVS` node (#11) so
    /// its baseline/comparison environments are picked from the loaded ones
    /// instead of typed. Returns `true` when the selection is an ENVS loop,
    /// `false` otherwise so the caller can try another form.
    fn open_report_node_envs(&mut self, idx: usize) -> bool {
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
        let (var, clause, parallel, body) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path) {
                Some(FlowNode::ForEnvs {
                    var,
                    clause,
                    parallel,
                    body,
                }) => (var.clone(), clause.clone(), *parallel, body.clone()),
                _ => return false, // not an ENVS loop
            }
        };
        let choices: Vec<String> = self.global_envs.iter().map(|e| e.name.clone()).collect();
        let snapshots = self.discover_report_snapshots(idx);
        // The `SHOW` checklist offers what the loop *body* reports, so it needs
        // the bound collection to ask each reported request what it emits.
        let selected_show = match &clause {
            crate::report::flow::EnvClause::Roles { baseline_show, .. } => baseline_show.clone(),
            crate::report::flow::EnvClause::Plain(_) => Vec::new(),
        };
        let show_choices = match self.resolve_bound_collection(&self.reports[idx].report) {
            Some(ci) => crate::report::edit::baseline_show_choices(
                &self.collections[ci].entries,
                &body,
                &selected_show,
            ),
            None => crate::report::edit::baseline_show_choices(&[], &body, &selected_show),
        };
        let form = EnvsForm::build(
            report_id,
            path,
            var,
            &clause,
            parallel,
            choices,
            snapshots,
            show_choices,
        );
        self.overlay = Some(Overlay::ReportNodeEnvs(Box::new(form)));
        true
    }

    /// List the `.baseline` snapshot files in report `idx`'s root directory as
    /// paths relative to that root — the candidates a `FILE(…)` role entry cycles
    /// through in the ENVS form. Relative so they match the `# root:`-relative
    /// resolution the runtime uses; empty on any I/O error (the form then just
    /// offers no snapshots, exactly like no loaded environments).
    fn discover_report_snapshots(&self, idx: usize) -> Vec<String> {
        let (root, _) = super::reports::report_base_dir(&self.reports[idx].report);
        let mut out: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "baseline")
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out
    }

    /// Finish an [`EnvsForm`]: rebuild the `FOR … IN ENVS` node from it (keeping
    /// the node's body untouched) and write it back. A no-op when the form
    /// describes no environments (so the node is never replaced by an
    /// unparseable empty clause). The `PARALLEL` marker is taken from the
    /// form's toggle (preserving any explicit `PARALLEL(n)` degree already on
    /// the node when the toggle stays on).
    pub(crate) fn apply_report_node_envs(&mut self, form: EnvsForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let Some(clause) = form.clause() else {
            return;
        };
        // Preserve the existing node's body; var, clause, the SHOW checklist and
        // the PARALLEL marker (including its degree) all come from the form.
        let body = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return;
            };
            match node_at(&flow, &form.path) {
                Some(FlowNode::ForEnvs { body, .. }) => body.clone(),
                _ => return,
            }
        };
        let node = FlowNode::ForEnvs {
            var: form.var_or_default(),
            clause,
            body,
            parallel: form.parallel_spec(),
        };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Open the `FOR … IN FILES` configure form for the selected node. Returns
    /// `true` when the selection is a single-variable `FILES` loop (so the
    /// caller stops trying other forms), `false` otherwise — a `FOLDERS` loop or
    /// a tuple-pattern loop falls through to the plain folder browser.
    fn open_report_node_files(&mut self, idx: usize) -> bool {
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
        let (var, dir, glob, parallel, folders, roles) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path) {
                Some(FlowNode::ForEach {
                    pattern,
                    producer: Producer::Files { dir, glob },
                    parallel,
                    ..
                }) if pattern.is_single() => (
                    pattern.named().next().unwrap_or("FILE").to_string(),
                    dir.clone(),
                    glob.clone(),
                    *parallel,
                    false,
                    Vec::new(),
                ),
                // `FOLDERS` shares the form: same variable, same folder picker,
                // same PARALLEL rows — it just has no `MATCH` glob.
                Some(FlowNode::ForEach {
                    pattern,
                    producer: Producer::Folders { dir, roles },
                    parallel,
                    ..
                }) if pattern.is_single() => (
                    pattern.named().next().unwrap_or("FOLDER").to_string(),
                    dir.clone(),
                    None,
                    *parallel,
                    true,
                    roles.clone(),
                ),
                _ => return false, // not a single-var FILES/FOLDERS loop
            }
        };
        let form = FilesForm::build(report_id, path, var, dir, glob, parallel, folders, roles);
        self.overlay = Some(Overlay::ReportNodeFiles(Box::new(form)));
        true
    }

    /// Finish a [`FilesForm`]: rebuild the `FOR … IN FILES` node from it
    /// (keeping the node's body untouched) and write it back.
    pub(crate) fn apply_report_node_files(&mut self, form: &FilesForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let body = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return;
            };
            match node_at(&flow, &form.path) {
                Some(FlowNode::ForEach { body, .. }) => body.clone(),
                _ => return,
            }
        };
        let node = FlowNode::ForEach {
            pattern: Pattern::single(form.var_or_default()),
            producer: form.producer(),
            body,
            parallel: form.parallel_spec(),
        };
        self.apply_node_replace(idx, &form.path, node);
    }

    /// Key handling for the FILES configure form ([`Overlay::ReportNodeFiles`]).
    /// ↑/↓ (or Tab) move between rows; the Var/Match rows take typed characters;
    /// the Folder row opens the file picker (applying the form's other fields
    /// first so they aren't lost); the Parallel row toggles with Space/←/→;
    /// Enter applies, Esc cancels.
    pub(crate) fn report_node_files_key_handler(
        &mut self,
        key: KeyEvent,
        mut form: Box<FilesForm>,
    ) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeFiles(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                if rows.get(sel).copied() == Some(FilesRow::Folder) {
                    // Persist the rest of the form, then hand off to the folder
                    // picker (which writes the chosen dir back into this node).
                    self.apply_report_node_files(&form);
                    self.open_files_form_folder(&form);
                } else {
                    self.apply_report_node_files(&form);
                }
            }
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    Some(FilesRow::Var) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_alphanumeric() || c == '_' => form.var.push(c),
                            KeyCode::Backspace => {
                                form.var.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(FilesRow::Match) => {
                        match key.code {
                            KeyCode::Char(c) => form.glob.push(c),
                            KeyCode::Backspace => {
                                form.glob.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(FilesRow::Folder) => {
                        if matches!(key.code, KeyCode::Char(' ')) {
                            self.apply_report_node_files(&form);
                            self.open_files_form_folder(&form);
                        } else {
                            keep(self, form);
                        }
                    }
                    Some(FilesRow::Parallel) => {
                        if matches!(
                            key.code,
                            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                        ) {
                            form.toggle_parallel();
                            // Turning PARALLEL off hides the degree row below.
                            form.selected = form.selected.min(form.last_row());
                        }
                        keep(self, form);
                    }
                    // The max-concurrency box: digits only.
                    Some(FilesRow::Degree) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_ascii_digit() => form.degree.push(c),
                            KeyCode::Backspace => {
                                form.degree.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }

    /// Park the FILES node and open the folder browser to pick its source
    /// directory (reusing the same [`crate::tui::app::FileAction::PickReportNodeFolder`]
    /// flow the plain folder key uses), seeded to the loop's current folder.
    fn open_files_form_folder(&mut self, form: &FilesForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let start = {
            let p = std::path::Path::new(&form.dir);
            if !form.dir.trim().is_empty() && p.is_dir() {
                Some(p.to_path_buf())
            } else if let Some(base) = self.active_report_base_dir() {
                let joined = base.join(&form.dir);
                Some(if joined.is_dir() { joined } else { base })
            } else {
                None
            }
        };
        if let Some(dir) = start {
            self.last_browse_dir = Some(dir);
        }
        self.pending_node_folder = Some((form.report_id, form.path.clone()));
        let _ = idx;
        self.open_browser(crate::tui::app::FileAction::PickReportNodeFolder);
    }

    /// Key handling for the ENVS configure form ([`Overlay::ReportNodeEnvs`]).
    /// ↑/↓ (or Tab) move between rows; the Var row takes identifier characters;
    /// the Mode row toggles Iterate/Compare with Space/←/→; env rows cycle the
    /// environment (or snapshot, for a `FILE` entry) with Space/←/→, set the
    /// baseline with `b`, toggle a `FILE(…)` snapshot reference with `f`, add
    /// with `n` and remove with `x`/Del; Enter applies, Esc cancels.
    pub(crate) fn report_node_envs_key_handler(&mut self, key: KeyEvent, mut form: Box<EnvsForm>) {
        let keep = |app: &mut TuiApp, form| {
            app.overlay = Some(Overlay::ReportNodeEnvs(form));
        };
        let last = form.last_row();
        match key.code {
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
                keep(self, form);
            }
            KeyCode::Down | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(last);
                keep(self, form);
            }
            KeyCode::Enter => self.apply_report_node_envs(*form),
            KeyCode::Esc => {} // cancel (overlay stays taken)
            _ => {
                let rows = form.visible_rows();
                let sel = form.selected.min(rows.len().saturating_sub(1));
                match rows.get(sel).copied() {
                    Some(EnvsRow::Var) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_alphanumeric() || c == '_' => form.var.push(c),
                            KeyCode::Backspace => {
                                form.var.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(EnvsRow::Mode) => {
                        if matches!(
                            key.code,
                            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                        ) {
                            form.toggle_mode();
                        }
                        keep(self, form);
                    }
                    Some(EnvsRow::Parallel) => {
                        if matches!(
                            key.code,
                            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                        ) {
                            form.toggle_parallel();
                            // Turning PARALLEL off hides the degree row, which
                            // can leave the selection past the end.
                            form.selected = form.selected.min(form.last_row());
                        }
                        keep(self, form);
                    }
                    // The max-concurrency box: digits only, so it can never
                    // hold something that won't serialize as `PARALLEL(n)`.
                    Some(EnvsRow::Degree) => {
                        match key.code {
                            KeyCode::Char(c) if c.is_ascii_digit() => form.degree.push(c),
                            KeyCode::Backspace => {
                                form.degree.pop();
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    Some(EnvsRow::BaselineShow(fi)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some(row) = form.baseline_show.get_mut(fi)
                        {
                            row.included = !row.included;
                        }
                        keep(self, form);
                    }
                    Some(EnvsRow::Env(ei)) => {
                        match key.code {
                            KeyCode::Char(' ') | KeyCode::Right => form.cycle_entry(ei, true),
                            KeyCode::Left => form.cycle_entry(ei, false),
                            KeyCode::Char('b') => form.toggle_baseline(ei),
                            KeyCode::Char('f') => form.toggle_file(ei),
                            KeyCode::Char('n') => {
                                form.add_entry();
                                form.selected = form.last_row();
                            }
                            KeyCode::Char('x') | KeyCode::Delete => {
                                form.remove_entry(ei);
                                form.selected = form.selected.min(form.last_row());
                            }
                            _ => {}
                        }
                        keep(self, form);
                    }
                    None => keep(self, form),
                }
            }
        }
    }
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
                    // A SHOW field checkbox.
                    Some(FormRow::Field(fi)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some(row) = form.fields.get_mut(fi)
                        {
                            row.included = !row.included;
                        }
                        keep(self, form);
                    }
                    // A HIDE field checkbox.
                    Some(FormRow::Hidden(fi)) => {
                        if matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('x'))
                            && let Some(row) = form.hide_fields.get_mut(fi)
                        {
                            row.included = !row.included;
                        }
                        keep(self, form);
                    }
                    // A WITH field: Space/Enter would both mean "open", but
                    // Enter is already "apply the whole form", so Space opens
                    // the field editor and `x`/Del removes the field outright.
                    Some(FormRow::With(wi)) => match key.code {
                        KeyCode::Char(' ') => {
                            let existing = form.with.get(wi).cloned();
                            let sub = WithFieldForm::build(
                                form.report_id,
                                form.path.clone(),
                                Some(wi),
                                existing.as_ref(),
                            );
                            // The parent form is applied first so the rows the
                            // user already changed aren't lost behind the
                            // sub-form.
                            self.apply_report_node_request(*form);
                            self.overlay = Some(Overlay::ReportNodeWithField(Box::new(sub)));
                        }
                        KeyCode::Char('x') | KeyCode::Delete => {
                            if wi < form.with.len() {
                                form.with.remove(wi);
                            }
                            form.selected = form.selected.min(form.last_row());
                            keep(self, form);
                        }
                        _ => keep(self, form),
                    },
                    Some(FormRow::AddWith) => {
                        if matches!(key.code, KeyCode::Char(' ')) {
                            let sub =
                                WithFieldForm::build(form.report_id, form.path.clone(), None, None);
                            self.apply_report_node_request(*form);
                            self.overlay = Some(Overlay::ReportNodeWithField(Box::new(sub)));
                        } else {
                            keep(self, form);
                        }
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
            self.apply_node_insert(idx, &menu.pos, node);
            // Land the freshly-inserted node straight in its most helpful
            // editor — the very view Enter would open on it. `apply_node_insert`
            // already selected the new node, so `configure_selected_node` routes
            // on its kind: the ENVS baseline/comparison/mode popup for a
            // `FOR … IN ENVS` loop, the source-folder browser for FILES/FOLDERS,
            // and the raw line editor for the kinds without a dedicated form yet
            // (ReportVar / Assign / List).
            self.configure_selected_node(idx);
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
    let focused = app.report_body_focused();
    let title = format!("{} — {}", s.report_nodes_heading, s.report_nodes_hint);
    let block = panel(title, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.report_pane_areas[crate::tui::reports::ReportPane::Source.idx()] = Rect::default();
    app.report_pane_bars[crate::tui::reports::ReportPane::Source.idx()] = Rect::default();
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
    app.push_mouse_hit(
        MouseLayer::Base,
        inner,
        MouseHitTarget::Scroll(MouseScrollTarget::ReportPane(
            crate::tui::reports::ReportPane::Source,
        )),
    );
    for row in first..rows.len().min(first + h) {
        app.push_mouse_hit(
            MouseLayer::Base,
            Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
            MouseHitTarget::ReportNodeRow(row),
        );
    }

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

    #[test]
    fn envs_form_round_trip_preserves_baseline_show() {
        // Build an EnvsForm from a Roles clause that carries SHOW(Time).
        let clause = EnvClause::Roles {
            baseline: vec![RoleRef::Env("prod".into())],
            comparisons: vec![RoleRef::Env("staging".into())],
            baseline_show: vec!["Time".into()],
        };
        let form = EnvsForm::build(1, vec![], "T".into(), &clause, None, vec![], vec![], vec![]);
        assert_eq!(
            form.selected_baseline_show(),
            vec!["Time".to_string()],
            "a SHOW field the clause names must come back ticked"
        );

        // clause() must hand it back intact — no silent drop.
        let rebuilt = form.clause().expect("clause must be Some");
        assert_eq!(
            rebuilt,
            EnvClause::Roles {
                baseline: vec![RoleRef::Env("prod".into())],
                comparisons: vec![RoleRef::Env("staging".into())],
                baseline_show: vec!["Time".into()],
            }
        );
    }

    #[test]
    fn envs_form_preserves_and_rebuilds_a_file_role() {
        // A FILE(…) role must survive a build → clause() round-trip, and its path
        // must be reachable in the snapshot cycle even when not on disk.
        let clause = EnvClause::Roles {
            baseline: vec![RoleRef::File("prod.baseline".into())],
            comparisons: vec![RoleRef::Env("staging".into())],
            baseline_show: vec![],
        };
        let form = EnvsForm::build(1, vec![], "T".into(), &clause, None, vec![], vec![], vec![]);
        assert!(
            form.snapshots.iter().any(|s| s == "prod.baseline"),
            "existing FILE path must be seeded into the cycle"
        );
        let rebuilt = form.clause().expect("clause must be Some");
        assert_eq!(rebuilt, clause);
    }

    #[test]
    fn envs_form_toggle_file_switches_a_role_to_a_snapshot() {
        // Toggling `f` on an env entry makes it a FILE role that picks the first
        // discovered snapshot; toggling back returns it to a live env.
        let clause = EnvClause::Roles {
            baseline: vec![RoleRef::Env("prod".into())],
            comparisons: vec![RoleRef::Env("staging".into())],
            baseline_show: vec![],
        };
        let mut form = EnvsForm::build(
            1,
            vec![],
            "T".into(),
            &clause,
            None,
            vec!["prod".into(), "staging".into()],
            vec!["snap.baseline".into()],
            vec![],
        );
        form.toggle_file(0);
        assert!(form.entries[0].file);
        assert_eq!(form.entries[0].name, "snap.baseline");
        match form.clause().expect("clause") {
            EnvClause::Roles { baseline, .. } => {
                assert_eq!(baseline, vec![RoleRef::File("snap.baseline".into())]);
            }
            other => panic!("expected roles, got {other:?}"),
        }
        form.toggle_file(0);
        assert!(!form.entries[0].file);
        assert_eq!(form.entries[0].name, "prod");
    }
}
