//! Self-contained model and behaviour for the wizard's "KVD" sections —
//! Headers, Cookies and Queries. All three are the same shape: a titled table
//! with an "+ Add …" row and a list of `Enabled / Key / Value / Description`
//! rows. Rather than carry three near-identical copies of the state, helper
//! methods and key handling, they share this one module: a [`KvdKind`]
//! discriminant picks which section a given field/row belongs to, and a
//! [`KvdSection`] bundles one section's rows together with its per-table view
//! state (Description-column visibility and scroll offset).
//!
//! Only the *shared* skeleton lives here. The Form section deliberately does
//! not: it has extra columns (Kind/Content-Type/Base64 Prefix), width- and
//! kind-dependent column visibility, cross-row column retargeting and three
//! dropdowns, so folding it in would trade the duplication for a thicket of
//! conditionals. The generic table *drawing* stays next to the other sections'
//! drawing in `wizard.rs` (it reuses table helpers shared with Form /
//! Asserts / Captures); this module is the data model and input handling.

use std::cell::Cell;
use std::ops::{Deref, DerefMut};

use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEvent;

use crate::i18n::Strings;

use super::super::editor::Editor;
use super::wizard::{NewField, NewReq, WizardTab};

/// Which of the three identical `Enabled/Key/Value/Description` sections a
/// field or row belongs to. Carried inside [`NewField::Kvd`] /
/// [`NewField::AddKvd`] so a single code path serves all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KvdKind {
    Header,
    Cookie,
    Query,
    Options,
}

impl KvdKind {
    /// The kinds in their fixed display order (Headers, Cookies, Queries, then
    /// the request `[Options]`) — used to drive shared iteration.
    pub(crate) const ALL: [KvdKind; 4] = [
        KvdKind::Header,
        KvdKind::Cookie,
        KvdKind::Query,
        KvdKind::Options,
    ];

    /// The `NewField` for a given row/column of this section.
    pub(crate) fn field(self, i: usize, col: HdrCol) -> NewField {
        NewField::Kvd(self, i, col)
    }

    /// The `NewField` for this section's "+ Add …" row.
    pub(crate) fn add_field(self) -> NewField {
        NewField::AddKvd(self)
    }

    /// The section-view tab this section is shown under.
    pub(crate) fn wizard_tab(self) -> WizardTab {
        match self {
            KvdKind::Header => WizardTab::Headers,
            KvdKind::Cookie => WizardTab::Cookies,
            KvdKind::Query => WizardTab::Queries,
            KvdKind::Options => WizardTab::Options,
        }
    }

    /// This section's title (`Headers` / `Cookies` / `Queries` / `Options`).
    pub(crate) fn title(self, s: &Strings) -> &'static str {
        match self {
            KvdKind::Header => s.field_headers,
            KvdKind::Cookie => s.field_cookies,
            KvdKind::Query => s.field_queries,
            KvdKind::Options => s.field_options,
        }
    }

    /// This section's "+ Add …" row label.
    pub(crate) fn add_label(self, s: &Strings) -> &'static str {
        match self {
            KvdKind::Header => s.add_header,
            KvdKind::Cookie => s.add_cookie,
            KvdKind::Query => s.add_query,
            KvdKind::Options => s.add_option,
        }
    }
}

/// Which column of a header-like row is focused. `Enabled` is the send toggle,
/// `Desc` is documentation only.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum HdrCol {
    Key,
    Value,
    Desc,
    Enabled,
}

/// One editable header-like row (used by Headers, Cookies and Queries). `desc`
/// is documentation only and is not sent; a row is only sent when `enabled`.
pub(crate) struct HeaderRow {
    pub(crate) key: Editor,
    pub(crate) value: Editor,
    pub(crate) desc: Editor,
    pub(crate) enabled: bool,
}

impl HeaderRow {
    pub(crate) fn new() -> Self {
        Self {
            key: Editor::blank(),
            value: Editor::blank(),
            desc: Editor::blank(),
            enabled: true,
        }
    }

    /// True when the row carries no text (its checkbox state is ignored).
    pub(crate) fn is_blank(&self) -> bool {
        self.key.text().is_empty() && self.value.text().is_empty() && self.desc.text().is_empty()
    }

    pub(crate) fn cell_mut(&mut self, col: HdrCol) -> Option<&mut Editor> {
        match col {
            HdrCol::Key => Some(&mut self.key),
            HdrCol::Value => Some(&mut self.value),
            HdrCol::Desc => Some(&mut self.desc),
            HdrCol::Enabled => None,
        }
    }

    pub(crate) fn cell(&self, col: HdrCol) -> Option<&Editor> {
        match col {
            HdrCol::Key => Some(&self.key),
            HdrCol::Value => Some(&self.value),
            HdrCol::Desc => Some(&self.desc),
            HdrCol::Enabled => None,
        }
    }
}

/// One `Enabled/Key/Value/Description` section (Headers, Cookies or Queries):
/// its rows plus the per-table view state that used to be tracked in separate
/// `*_desc_visible` / `*_scroll` fields. Derefs to its row `Vec` so existing
/// `.len()` / `[i]` / `.push()` / `.iter()` call sites keep working unchanged.
pub(crate) struct KvdSection {
    pub(crate) rows: Vec<HeaderRow>,
    /// Set during draw: whether the Description column currently has room.
    /// Focus navigation skips Description cells when it does not.
    pub(crate) desc_visible: Cell<bool>,
    /// Index of the first visible row, updated at draw time to keep the
    /// focused row in view once the section has more rows than fit on screen.
    pub(crate) scroll: Cell<usize>,
}

impl KvdSection {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            desc_visible: Cell::new(true),
            scroll: Cell::new(0),
        }
    }

    /// Build a section from pre-populated rows (used when prefilling the Edit
    /// Request overlay from an existing entry).
    pub(crate) fn from_rows(rows: Vec<HeaderRow>) -> Self {
        Self {
            rows,
            ..Self::new()
        }
    }

    /// True when every row is blank — the whole section is then skipped when
    /// tabbing past it. Vacuously true when the section has no rows.
    pub(crate) fn is_blank(&self) -> bool {
        self.rows.iter().all(HeaderRow::is_blank)
    }

    /// All columns of a row, in left-to-right visual order, for arrow-key
    /// navigation. The Enabled checkbox comes first, matching its position as
    /// the leftmost column on screen — so `Left` from Key reaches it directly
    /// instead of wrapping all the way round. Description is omitted when its
    /// column is too narrow to be shown.
    pub(crate) fn row_cells(&self) -> Vec<HdrCol> {
        if self.desc_visible.get() {
            vec![HdrCol::Enabled, HdrCol::Key, HdrCol::Value, HdrCol::Desc]
        } else {
            vec![HdrCol::Enabled, HdrCol::Key, HdrCol::Value]
        }
    }

    /// Columns visited by Tab / Shift+Tab within a row. The Enabled checkbox
    /// is intentionally excluded — it is reached with the arrow keys or by
    /// pressing Ctrl+E. A brand new row always starts focus on Key regardless
    /// of this order (set explicitly when the row is created).
    pub(crate) fn tab_cells(&self) -> Vec<HdrCol> {
        if self.desc_visible.get() {
            vec![HdrCol::Key, HdrCol::Value, HdrCol::Desc]
        } else {
            vec![HdrCol::Key, HdrCol::Value]
        }
    }

    /// The column to the left of `col` within a row, if any.
    pub(crate) fn prev_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        idx.checked_sub(1).map(|p| cells[p])
    }

    /// The column to the right of `col` within a row, if any.
    pub(crate) fn next_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        cells.get(idx + 1).copied()
    }
}

impl Default for KvdSection {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for KvdSection {
    type Target = Vec<HeaderRow>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl DerefMut for KvdSection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rows
    }
}

impl NewReq {
    /// Shared read access to one of the three KVD sections.
    pub(crate) fn kvd(&self, kind: KvdKind) -> &KvdSection {
        match kind {
            KvdKind::Header => &self.headers,
            KvdKind::Cookie => &self.cookies,
            KvdKind::Query => &self.queries,
            KvdKind::Options => &self.options,
        }
    }

    /// Shared mutable access to one of the three KVD sections.
    pub(crate) fn kvd_mut(&mut self, kind: KvdKind) -> &mut KvdSection {
        match kind {
            KvdKind::Header => &mut self.headers,
            KvdKind::Cookie => &mut self.cookies,
            KvdKind::Query => &mut self.queries,
            KvdKind::Options => &mut self.options,
        }
    }

    /// True when every row of `kind`'s section is blank — the section is then
    /// skipped when tabbing past it.
    pub(crate) fn kvd_blank(&self, kind: KvdKind) -> bool {
        self.kvd(kind).is_blank()
    }

    /// The field that represents "arriving at `kind`'s section": its first
    /// row when one exists, or the "+ Add …" row when the section is empty
    /// (there's no default blank row to land on).
    pub(crate) fn kvd_entry(&self, kind: KvdKind) -> NewField {
        if self.kvd(kind).is_empty() {
            kind.add_field()
        } else {
            kind.field(0, HdrCol::Key)
        }
    }

    /// Where Up-arrow lands when leaving the first row of the *next* section
    /// upward into `kind`'s section: its last row if any exist, otherwise its
    /// "+ Add …" row — arrow-key row navigation stops at every section exactly
    /// like Down does, so an empty section is never skipped over.
    pub(crate) fn up_into_kvd(&self, kind: KvdKind) -> NewField {
        let sec = self.kvd(kind);
        if sec.is_empty() {
            kind.add_field()
        } else {
            kind.field(sec.len() - 1, HdrCol::Key)
        }
    }

    /// Where Up-arrow lands when leaving the first row of `kind`'s section
    /// upward: the section immediately above it (URL above Headers, the last
    /// Header row above Cookies, the last Cookie row above Queries, the last
    /// Query row above Options).
    fn kvd_up_destination(&self, kind: KvdKind) -> NewField {
        match kind {
            KvdKind::Header => NewField::Url,
            KvdKind::Cookie => self.up_into_kvd(KvdKind::Header),
            KvdKind::Query => self.up_into_kvd(KvdKind::Cookie),
            KvdKind::Options => self.up_into_kvd(KvdKind::Query),
        }
    }

    /// Key handling for a focused cell of any KVD section — the single code
    /// path that used to be three near-identical `match` arms (one each for
    /// Headers, Cookies and Queries). `i`/`col` identify the focused cell.
    pub(crate) fn handle_kvd_key(&mut self, kind: KvdKind, i: usize, col: HdrCol, key: &KeyEvent) {
        let ctrl = key
            .modifiers
            .contains(ratatui::crossterm::event::KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => {
                // Move up a row, or leave the table upward to the section
                // above when already on the first row.
                self.focus = if i > 0 {
                    kind.field(i - 1, col)
                } else {
                    self.kvd_up_destination(kind)
                };
            }
            KeyCode::Down => {
                // Move down a row, or leave the table downward to the
                // "+ Add …" row when on the last row.
                self.focus = if i + 1 < self.kvd(kind).len() {
                    kind.field(i + 1, col)
                } else {
                    kind.add_field()
                };
            }
            KeyCode::Left => {
                let at_start = self.kvd(kind)[i]
                    .cell(col)
                    .map(|ed| ed.col == 0)
                    .unwrap_or(true);
                if !at_start {
                    if let Some(ed) = self.kvd_mut(kind)[i].cell_mut(col) {
                        if ctrl {
                            ed.home()
                        } else {
                            ed.left();
                        }
                    }
                } else if let Some(prev) = self.kvd(kind).prev_col(col) {
                    if let Some(ed) = self.kvd_mut(kind)[i].cell_mut(prev) {
                        ed.end();
                    }
                    self.focus = kind.field(i, prev);
                }
            }
            KeyCode::Right => {
                let at_end = self.kvd(kind)[i]
                    .cell(col)
                    .map(|ed| ed.col >= ed.line_len(ed.row))
                    .unwrap_or(true);
                if !at_end {
                    if let Some(ed) = self.kvd_mut(kind)[i].cell_mut(col) {
                        if ctrl {
                            ed.end();
                        } else {
                            ed.right();
                        }
                    }
                } else if let Some(next) = self.kvd(kind).next_col(col) {
                    if let Some(ed) = self.kvd_mut(kind)[i].cell_mut(next) {
                        ed.home();
                    }
                    self.focus = kind.field(i, next);
                }
            }
            KeyCode::Enter => self.focus_next(true, true),
            KeyCode::Char(' ') if col == HdrCol::Enabled => {
                if let Some(row) = self.kvd_mut(kind).get_mut(i) {
                    row.enabled = !row.enabled;
                }
            }
            _ => {
                if let Some(ed) = self.kvd_mut(kind)[i].cell_mut(col) {
                    match key.code {
                        KeyCode::Char(ch) => ed.insert(ch),
                        KeyCode::Backspace => ed.backspace(),
                        KeyCode::Home => ed.home(),
                        KeyCode::End => ed.end(),
                        _ => {}
                    }
                }
            }
        }
    }
}
