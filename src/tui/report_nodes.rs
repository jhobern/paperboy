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
    EnvClause, FlowNode, Pattern, Producer, ReportStmt, ResponseFmt, RoleRef, WithItem,
};

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
    /// The node's `HIDE(…)` clause, preserved verbatim across an edit (the
    /// form doesn't expose a HIDE editor, but must not drop the clause when
    /// re-serializing).
    pub(crate) hide: Vec<String>,
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
            hide,
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

/// One row of the [`EnvsForm`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvsRow {
    /// The loop variable name (a free identifier, editable inline).
    Var,
    /// The Iterate (`Plain`) vs Compare (`Roles`) mode toggle.
    Mode,
    /// The `PARALLEL` on/off toggle (run iterations concurrently).
    Parallel,
    /// One environment entry (index into [`EnvsForm::entries`]).
    Env(usize),
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
    /// Field suffixes from `BASELINE(…) SHOW(…)`.  The form has no editing UI
    /// for this — it is preserved verbatim so a round-trip doesn't silently drop
    /// a `SHOW(…)` clause the user wrote in source.
    pub(crate) baseline_show: Vec<String>,
}

impl EnvsForm {
    /// Build the form from a node's current variable and [`EnvClause`].
    /// `choices` are the loaded environment names an env entry cycles through;
    /// `snapshots` are the discovered `.baseline` paths a `FILE(…)` entry cycles.
    fn build(
        report_id: u64,
        path: Vec<usize>,
        var: String,
        clause: &EnvClause,
        parallel: bool,
        choices: Vec<String>,
        mut snapshots: Vec<String>,
    ) -> Self {
        let (compare, mut entries, baseline_show) = match clause {
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
        EnvsForm {
            report_id,
            path,
            var,
            compare,
            parallel,
            entries,
            choices,
            snapshots,
            selected: 0,
            baseline_show,
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<EnvsRow> {
        let mut rows = vec![EnvsRow::Var, EnvsRow::Mode, EnvsRow::Parallel];
        rows.extend((0..self.entries.len()).map(EnvsRow::Env));
        rows
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
                baseline_show: self.baseline_show.clone(),
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
}

/// The `FOR … IN FILES` configure form ([`Overlay::ReportNodeFiles`]), reached
/// with Enter on a `FILES` loop node — the file analogue of [`EnvsForm`]. It
/// picks the loop variable, the source folder (via the file picker), an
/// optional `MATCH` glob, and whether the loop runs `PARALLEL`.
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
    /// Selected row: an index into [`Self::visible_rows`] (clamped on use).
    pub(crate) selected: usize,
}

impl FilesForm {
    /// Build the form from a `FILES` loop's current variable, directory, glob
    /// and parallel marker. A freshly-inserted loop (empty `dir`) starts with
    /// the Folder row selected so the picker is one keystroke away — the source
    /// directory is the whole point of the loop.
    fn build(
        report_id: u64,
        path: Vec<usize>,
        var: String,
        dir: String,
        glob: Option<String>,
        parallel: bool,
    ) -> Self {
        let selected = if dir.trim().is_empty() { 1 } else { 0 };
        FilesForm {
            report_id,
            path,
            var,
            dir,
            glob: glob.unwrap_or_default(),
            parallel,
            selected,
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<FilesRow> {
        vec![
            FilesRow::Var,
            FilesRow::Folder,
            FilesRow::Match,
            FilesRow::Parallel,
        ]
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
        if self.open_report_node_envs(idx) {
            return;
        }
        if self.open_report_node_files(idx) {
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
        let (name, report, alias, response, current_show, current_hide, with) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path) {
                Some(FlowNode::Request { name }) => (
                    name.clone(),
                    false,
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ),
                Some(FlowNode::Report(ReportStmt::Request {
                    name,
                    alias,
                    response_fmt,
                    show,
                    hide,
                    with,
                })) => (
                    name.clone(),
                    true,
                    alias.clone(),
                    *response_fmt,
                    show.clone(),
                    hide.clone(),
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
            current_hide,
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
                hide: form.hide.clone(),
                with: form.with.clone(),
            })
        } else {
            FlowNode::Request {
                name: form.request.clone(),
            }
        };
        self.apply_node_replace(idx, &form.path, node);
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
        let (var, clause, parallel) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return false;
            };
            match node_at(&flow, &path) {
                Some(FlowNode::ForEnvs {
                    var,
                    clause,
                    parallel,
                    ..
                }) => (var.clone(), clause.clone(), parallel.is_some()),
                _ => return false, // not an ENVS loop
            }
        };
        let choices: Vec<String> = self.global_envs.iter().map(|e| e.name.clone()).collect();
        let snapshots = self.discover_report_snapshots(idx);
        let form = EnvsForm::build(report_id, path, var, &clause, parallel, choices, snapshots);
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
        // Preserve the existing node's body and any explicit PARALLEL degree;
        // only var/clause and the parallel on/off state change here.
        let (body, existing_parallel) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return;
            };
            match node_at(&flow, &form.path) {
                Some(FlowNode::ForEnvs { body, parallel, .. }) => (body.clone(), *parallel),
                _ => return,
            }
        };
        let parallel = if form.parallel {
            Some(existing_parallel.unwrap_or_default())
        } else {
            None
        };
        let node = FlowNode::ForEnvs {
            var: form.var_or_default(),
            clause,
            body,
            parallel,
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
        let (var, dir, glob, parallel) = {
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
                    parallel.is_some(),
                ),
                _ => return false, // not a single-var FILES loop
            }
        };
        let form = FilesForm::build(report_id, path, var, dir, glob, parallel);
        self.overlay = Some(Overlay::ReportNodeFiles(Box::new(form)));
        true
    }

    /// Finish a [`FilesForm`]: rebuild the `FOR … IN FILES` node from it
    /// (keeping the node's body untouched) and write it back.
    pub(crate) fn apply_report_node_files(&mut self, form: &FilesForm) {
        let Some(idx) = self.report_index_by_id(form.report_id) else {
            return;
        };
        let (body, existing_parallel) = {
            let Ok(flow) = self.reports[idx].report.flow() else {
                return;
            };
            match node_at(&flow, &form.path) {
                Some(FlowNode::ForEach { body, parallel, .. }) => (body.clone(), *parallel),
                _ => return,
            }
        };
        let parallel = if form.parallel {
            Some(existing_parallel.unwrap_or_default())
        } else {
            None
        };
        let node = FlowNode::ForEach {
            pattern: Pattern::single(form.var_or_default()),
            producer: Producer::Files {
                dir: form.dir.clone(),
                glob: form.glob_opt(),
            },
            body,
            parallel,
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
        let form = EnvsForm::build(1, vec![], "T".into(), &clause, false, vec![], vec![]);
        assert_eq!(form.baseline_show, vec!["Time".to_string()]);

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
        let form = EnvsForm::build(1, vec![], "T".into(), &clause, false, vec![], vec![]);
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
            false,
            vec!["prod".into(), "staging".into()],
            vec!["snap.baseline".into()],
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
