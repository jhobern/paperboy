use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};

use crate::hurl::{FormFieldKind, METHODS};
use crate::i18n::Strings;

use super::draw::*;
use super::editor::*;
use super::theme::*;

/// Common HTTP header names offered as autocomplete suggestions for the Key
/// field of the New Request headers table. Kept in a sensible display order.
pub(crate) const COMMON_HEADERS: &[&str] = &[
    "Accept",
    "Accept-Charset",
    "Accept-Encoding",
    "Accept-Language",
    "Authorization",
    "Cache-Control",
    "Connection",
    "Content-Length",
    "Content-Type",
    "Cookie",
    "Date",
    "ETag",
    "Expect",
    "Host",
    "If-Match",
    "If-Modified-Since",
    "If-None-Match",
    "Origin",
    "Pragma",
    "Range",
    "Referer",
    "User-Agent",
    "X-Api-Key",
    "X-Content-Type-Options",
    "X-Correlation-ID",
    "X-CSRF-Token",
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "X-Frame-Options",
    "X-Request-ID",
    "X-Requested-With",
];

/// File-extension → MIME-type table offered by the content-type override
/// dropdown for `File`-kind Form/Multipart rows (see
/// https://hurl.dev/docs/request.html). Several extensions share a MIME type
/// (`jpg`/`jpeg`, `htm`/`html`); [`content_type_options`] de-duplicates those
/// for display while this table keeps every extension for lookup.
pub(crate) const CONTENT_TYPE_TABLE: &[(&str, &str)] = &[
    ("gif", "image/gif"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("svg", "image/svg+xml"),
    ("txt", "text/plain"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("pdf", "application/pdf"),
    ("xml", "application/xml"),
    ("webm", "video/webm"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("avi", "video/x-msvideo"),
    ("wmv", "video/x-ms-wmv"),
    ("mkv", "video/x-matroska"),
    ("mpeg", "video/mpeg"),
    ("mpg", "video/mpeg"),
    ("3gp", "video/3gpp"),
    ("ogv", "video/ogg"),
];

/// Infer a file's content type from its extension via [`CONTENT_TYPE_TABLE`]
/// (case-insensitive). `None` when the extension isn't recognised, in which
/// case Hurl falls back to `application/octet-stream` on its own.
pub(crate) fn infer_content_type(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    CONTENT_TYPE_TABLE
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| *m)
}

/// Common MIME types that aren't tied to any single file extension in
/// [`CONTENT_TYPE_TABLE`] — mostly API request-body encodings a user is
/// likely to want to force as an override even though there's no uploaded
/// file to infer them from. Offered by the content-type dropdown alongside
/// the extension-derived ones (see [`content_type_options`]).
pub(crate) const COMMON_CONTENT_TYPES: &[&str] = &[
    "application/json",
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "application/octet-stream",
    "text/css",
    "application/javascript",
    "text/csv",
    "application/x-yaml",
];

/// The unique MIME types offered by the content-type dropdown, in
/// alphabetical order: every entry in [`CONTENT_TYPE_TABLE`] (duplicates
/// like `jpg`/`jpeg`, `htm`/`html` collapsed) plus [`COMMON_CONTENT_TYPES`],
/// skipping any of those already covered by an extension (e.g. `text/plain`).
pub(crate) fn content_type_options() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, mime) in CONTENT_TYPE_TABLE {
        if !out.contains(mime) {
            out.push(mime);
        }
    }
    for mime in COMMON_CONTENT_TYPES {
        if !out.contains(mime) {
            out.push(mime);
        }
    }
    out.sort_unstable();
    out
}

/// [`content_type_options`] entries matching `query` (case-insensitive
/// substring, mirrors [`filter_headers`]). An empty query returns the full
/// list.
pub(crate) fn filter_content_types(query: &str) -> Vec<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return content_type_options();
    }
    content_type_options()
        .into_iter()
        .filter(|o| o.to_ascii_lowercase().contains(&q))
        .collect()
}

/// Common header names matching `query` (case-insensitive substring). An empty
/// query returns the full list.
pub(crate) fn filter_headers(query: &str) -> Vec<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return COMMON_HEADERS.to_vec();
    }
    COMMON_HEADERS
        .iter()
        .copied()
        .filter(|h| h.to_ascii_lowercase().contains(&q))
        .collect()
}

/// Which column of a header row is focused. `Enabled` is the send toggle,
/// `Desc` is documentation only.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum HdrCol {
    Key,
    Value,
    Desc,
    Enabled,
}

/// One editable header row. `desc` is documentation only and is not sent;
/// a row is only sent when `enabled`.
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
}

/// Which column of a `[Form]`/`[Multipart]` row is focused. Like [`HdrCol`]
/// but with an extra `Kind` (Text/File/Base64 File type) dropdown column and,
/// for `Base64File` rows, a `Prefix` column holding the text prepended to the
/// file's base64 encoding.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FormCol {
    Key,
    Value,
    Kind,
    Ctype,
    Prefix,
    Desc,
    Enabled,
}

/// One editable `[Form]`/`[Multipart]` row. `desc` is documentation-only (like
/// [`HeaderRow`]'s), never persisted. `ctype` is the optional Hurl
/// content-type override for `File`-kind rows (`key: file,path;content-type`)
/// and is persisted; it's ignored for `Text` rows. A row is only sent when
/// `enabled`.
pub(crate) struct FormRow {
    pub(crate) key: Editor,
    pub(crate) value: Editor,
    pub(crate) ctype: Editor,
    /// Text prepended to the file's base64 encoding for `Base64File` rows
    /// (e.g. `data:image/png;base64,`); persisted, ignored for other kinds.
    pub(crate) base64_prefix: Editor,
    pub(crate) desc: Editor,
    pub(crate) enabled: bool,
    /// Defaults to `Text` on a fresh row — with only two real options,
    /// requiring the user to explicitly pick one before continuing just adds
    /// friction for the common case, so the cell starts populated and its
    /// dropdown (Down/Up to flip, or `Enter` to browse) behaves like any
    /// other already-populated cell: it doesn't auto-open.
    pub(crate) kind: FormFieldKind,
}

impl FormRow {
    pub(crate) fn new() -> Self {
        Self {
            key: Editor::blank(),
            value: Editor::blank(),
            ctype: Editor::blank(),
            base64_prefix: Editor::blank(),
            desc: Editor::blank(),
            enabled: true,
            kind: FormFieldKind::Text,
        }
    }

    /// True when the row carries no text (its checkbox/kind state is ignored).
    pub(crate) fn is_blank(&self) -> bool {
        self.key.text().is_empty()
            && self.value.text().is_empty()
            && self.ctype.text().is_empty()
            && self.base64_prefix.text().is_empty()
            && self.desc.text().is_empty()
    }

    pub(crate) fn cell_mut(&mut self, col: FormCol) -> Option<&mut Editor> {
        match col {
            FormCol::Key => Some(&mut self.key),
            FormCol::Value => Some(&mut self.value),
            FormCol::Ctype => Some(&mut self.ctype),
            FormCol::Prefix => Some(&mut self.base64_prefix),
            FormCol::Desc => Some(&mut self.desc),
            FormCol::Kind | FormCol::Enabled => None,
        }
    }
}

/// Which column of a `[Captures]` row is focused: the variable name or the
/// query expression (e.g. `token: jsonpath "$.token"`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CapCol {
    Name,
    Expr,
}

/// One editable `[Captures]` row.
pub(crate) struct CaptureRow {
    pub(crate) name: Editor,
    pub(crate) expr: Editor,
}

impl CaptureRow {
    pub(crate) fn new() -> Self {
        Self {
            name: Editor::blank(),
            expr: Editor::blank(),
        }
    }

    /// True when the row carries no text.
    pub(crate) fn is_blank(&self) -> bool {
        self.name.text().is_empty() && self.expr.text().is_empty()
    }

    pub(crate) fn cell_mut(&mut self, col: CapCol) -> &mut Editor {
        match col {
            CapCol::Name => &mut self.name,
            CapCol::Expr => &mut self.expr,
        }
    }
}

/// One editable `[Asserts]` row: a raw Hurl assert expression, e.g.
/// `jsonpath "$.status" == "ok"`.
pub(crate) struct AssertRow {
    pub(crate) expr: Editor,
}

impl AssertRow {
    pub(crate) fn new() -> Self {
        Self {
            expr: Editor::blank(),
        }
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.expr.text().is_empty()
    }
}

/// Which field of the New/Edit Request form is focused.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum NewField {
    Name,
    Target,
    Method,
    Url,
    Header(usize, HdrCol),
    AddHeader,
    Cookie(usize, HdrCol),
    AddCookie,
    FormField(usize, FormCol),
    AddFormField,
    Body,
    Assert(usize),
    AddAssert,
    Capture(usize, CapCol),
    AddCapture,
}

impl NewField {
    /// Which section-view tab this field belongs to, if any. `Name`/`Target`/
    /// `Method`/`Url` return `None`: they're always shown above the tab bar
    /// regardless of which tab is active, so they aren't "in" any one
    /// section for navigation-confinement purposes.
    pub(crate) fn wizard_section(self) -> Option<WizardTab> {
        match self {
            NewField::Name | NewField::Target | NewField::Method | NewField::Url => None,
            NewField::Header(..) | NewField::AddHeader => Some(WizardTab::Headers),
            NewField::Cookie(..) | NewField::AddCookie => Some(WizardTab::Cookies),
            NewField::FormField(..) | NewField::AddFormField => Some(WizardTab::Form),
            NewField::Body => Some(WizardTab::Body),
            NewField::Assert(..) | NewField::AddAssert => Some(WizardTab::Asserts),
            NewField::Capture(..) | NewField::AddCapture => Some(WizardTab::Captures),
        }
    }
}

/// Which section-view tab of the Request wizard is active. `All` is the
/// default combined layout (every section stacked, as before); each other
/// variant devotes essentially the whole dialog body to just that one
/// section's table, so long lists of Headers/Asserts/etc. get far more
/// visible rows without any extra scrolling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WizardTab {
    All,
    Headers,
    Cookies,
    Form,
    Body,
    Asserts,
    Captures,
}

impl WizardTab {
    /// All tabs, in their default display order — used to initialize a
    /// fresh `NewReq::tab_order` (which the user may subsequently reorder).
    pub(crate) const ALL: [WizardTab; 7] = [
        WizardTab::All,
        WizardTab::Headers,
        WizardTab::Cookies,
        WizardTab::Form,
        WizardTab::Body,
        WizardTab::Asserts,
        WizardTab::Captures,
    ];

    pub(crate) fn label(self, s: &Strings) -> &'static str {
        match self {
            WizardTab::All => s.tab_all,
            WizardTab::Headers => s.field_headers,
            WizardTab::Cookies => s.field_cookies,
            WizardTab::Form => s.field_form,
            WizardTab::Body => s.field_body,
            WizardTab::Asserts => s.field_asserts,
            WizardTab::Captures => s.field_captures,
        }
    }

    /// The field to focus when this tab becomes active by navigating
    /// forward into it, or when confined navigation wraps forward past the
    /// end of the section back to its start: the top entry of the section
    /// (for `Body`, its only field — which is also how it ends up "in
    /// editing mode" automatically, since the Body editor renders in edit
    /// mode whenever it has focus).
    pub(crate) fn first_field(self) -> NewField {
        match self {
            WizardTab::All => NewField::Name,
            WizardTab::Headers => NewField::Header(0, HdrCol::Key),
            WizardTab::Cookies => NewField::Cookie(0, HdrCol::Key),
            WizardTab::Form => NewField::FormField(0, FormCol::Key),
            WizardTab::Body => NewField::Body,
            WizardTab::Asserts => NewField::Assert(0),
            WizardTab::Captures => NewField::Capture(0, CapCol::Name),
        }
    }

    /// The field confined backward navigation wraps to past the start of
    /// the section (its "Add …" row, or Body itself).
    pub(crate) fn last_field(self) -> NewField {
        match self {
            WizardTab::All => NewField::Name,
            WizardTab::Headers => NewField::AddHeader,
            WizardTab::Cookies => NewField::AddCookie,
            WizardTab::Form => NewField::AddFormField,
            WizardTab::Body => NewField::Body,
            WizardTab::Asserts => NewField::AddAssert,
            WizardTab::Captures => NewField::AddCapture,
        }
    }
}

/// In-progress New Request / Edit Request form (an overlay while open).
pub(crate) struct NewReq {
    pub(crate) name: Editor,
    pub(crate) url: Editor,
    pub(crate) headers: Vec<HeaderRow>,
    pub(crate) cookies: Vec<HeaderRow>,
    pub(crate) form_fields: Vec<FormRow>,
    pub(crate) body: Editor,
    pub(crate) asserts: Vec<AssertRow>,
    pub(crate) captures: Vec<CaptureRow>,
    pub(crate) method_idx: usize,
    pub(crate) focus: NewField,
    /// Target collection tab (index) the request will be added to, and the
    /// display names to cycle through.
    pub(crate) target_idx: usize,
    pub(crate) target_names: Vec<String>,
    /// The collection's Base URL, offered as a ghost default in the URL field.
    pub(crate) base_url: String,
    /// Highlighted index in the header-name suggestion dropdown, when the user
    /// is navigating it. `None` means "typing" (no suggestion highlighted).
    pub(crate) suggest_hi: Option<usize>,
    /// True once the user has dismissed the dropdown with Esc; reset when they
    /// type or move to a different field.
    pub(crate) suggest_hidden: bool,
    /// True once the user has dismissed the Form Kind (Text/File) dropdown
    /// with Esc; reset whenever focus moves to a different field, so it
    /// reopens (like `suggest_hidden`) the next time Kind is focused.
    pub(crate) kind_dropdown_hidden: bool,
    /// Like `kind_dropdown_hidden`, but for the content-type dropdown shown
    /// over a `File`-kind Form row's Content-Type cell.
    pub(crate) ctype_dropdown_hidden: bool,
    /// Highlighted index in the content-type dropdown (0 = "Auto", 1.. =
    /// `content_type_options()`); `None` while nothing is highlighted yet.
    pub(crate) ctype_hi: Option<usize>,
    /// Set during draw: whether the Description column currently has room.
    /// Focus navigation skips Description cells when it does not.
    pub(crate) desc_visible: std::cell::Cell<bool>,
    /// Set during draw: screen rect of the focused Key cell, so the suggestion
    /// dropdown can be anchored beneath it.
    pub(crate) key_cell_rect: std::cell::Cell<Option<Rect>>,
    /// Set during draw: screen rect of the focused Form Kind cell, so its
    /// Text/File dropdown can be anchored beneath it.
    pub(crate) kind_cell_rect: std::cell::Cell<Option<Rect>>,
    /// Set during draw: screen rect of the focused Form Content-Type cell
    /// (when its row is `File`-kind), so the content-type dropdown can be
    /// anchored beneath it.
    pub(crate) ctype_cell_rect: std::cell::Cell<Option<Rect>>,
    /// Like `desc_visible`, but for the Cookies table (a separate table with
    /// its own available width).
    pub(crate) cookie_desc_visible: std::cell::Cell<bool>,
    /// Like `desc_visible`, but for the Form table's Description column.
    pub(crate) form_desc_visible: std::cell::Cell<bool>,
    /// Set during draw: whether the Form table's Content-Type column
    /// currently has room. Focus navigation skips it when it does not.
    pub(crate) form_ctype_visible: std::cell::Cell<bool>,
    /// Set during draw: whether the Form table's Base64 Prefix column
    /// currently has room. Focus navigation skips it when it does not.
    pub(crate) form_prefix_visible: std::cell::Cell<bool>,
    /// Per-section scroll offset (index of the first visible row), updated at
    /// draw time to keep the focused row in view once a section has more rows
    /// than fit on screen.
    pub(crate) header_scroll: std::cell::Cell<usize>,
    pub(crate) cookie_scroll: std::cell::Cell<usize>,
    pub(crate) form_scroll: std::cell::Cell<usize>,
    pub(crate) assert_scroll: std::cell::Cell<usize>,
    pub(crate) capture_scroll: std::cell::Cell<usize>,
    /// Directory Form file paths are resolved against for the existence check
    /// (the saved collection's own directory, per Hurl's "relative to the
    /// input Hurl file" rule); `None` (Scratch Space, unsaved) resolves
    /// against the process's current directory instead.
    pub(crate) file_root: Option<PathBuf>,
    /// `Some((collection_idx, entry_idx))` when this form is editing an
    /// existing request (the Target field is then hidden — an existing entry
    /// stays in its original collection) rather than creating a new one.
    pub(crate) editing: Option<(usize, usize)>,
    /// Which section-view tab is active (`All` shows every section stacked,
    /// as before; any other variant devotes the whole body to just that
    /// section). Cycled with PageUp/PageDown; purely a view/layout choice —
    /// all tabs read and write the same fields above, so edits made while a
    /// section tab is active are immediately visible from every other tab.
    pub(crate) view_tab: WizardTab,
    /// Display/cycle order of the section-view tabs. Starts as
    /// `WizardTab::ALL` but the user can reorder it (via
    /// `Ctrl+Shift+Left/Right`, mirroring how collection tabs are
    /// reordered); `All` is always pinned first and can never move.
    pub(crate) tab_order: Vec<WizardTab>,
}

impl NewReq {
    pub(crate) fn new(
        base_url: String,
        target_names: Vec<String>,
        target_idx: usize,
        file_root: Option<PathBuf>,
    ) -> Self {
        Self {
            name: Editor::blank(),
            url: Editor::blank(),
            headers: Vec::new(),
            cookies: Vec::new(),
            form_fields: Vec::new(),
            body: Editor::new("", true),
            asserts: Vec::new(),
            captures: Vec::new(),
            method_idx: 0,
            focus: NewField::Name,
            target_idx: target_idx.min(target_names.len().saturating_sub(1)),
            target_names,
            base_url,
            suggest_hi: None,
            suggest_hidden: false,
            kind_dropdown_hidden: false,
            ctype_dropdown_hidden: false,
            ctype_hi: None,
            desc_visible: std::cell::Cell::new(true),
            key_cell_rect: std::cell::Cell::new(None),
            kind_cell_rect: std::cell::Cell::new(None),
            ctype_cell_rect: std::cell::Cell::new(None),
            cookie_desc_visible: std::cell::Cell::new(true),
            form_desc_visible: std::cell::Cell::new(true),
            form_ctype_visible: std::cell::Cell::new(true),
            form_prefix_visible: std::cell::Cell::new(true),
            header_scroll: std::cell::Cell::new(0),
            cookie_scroll: std::cell::Cell::new(0),
            form_scroll: std::cell::Cell::new(0),
            assert_scroll: std::cell::Cell::new(0),
            capture_scroll: std::cell::Cell::new(0),
            file_root,
            editing: None,
            view_tab: WizardTab::All,
            tab_order: WizardTab::ALL.to_vec(),
        }
    }

    /// Build a form prefilled from an existing entry, for the "Edit Request"
    /// overlay. `ci`/`ei` identify the entry being edited (its collection and
    /// index) so the commit can be applied back in place.
    pub(crate) fn from_entry(
        ci: usize,
        ei: usize,
        entry: &crate::hurl::HurlEntry,
        base_url: String,
        target_names: Vec<String>,
        file_root: Option<PathBuf>,
    ) -> Self {
        let method_idx = METHODS.iter().position(|m| *m == entry.method).unwrap_or(0);
        let headers = if entry.headers.is_empty() {
            Vec::new()
        } else {
            entry
                .headers
                .iter()
                .map(|(k, v)| {
                    let mut row = HeaderRow::new();
                    row.key = Editor::new(k, false);
                    row.value = Editor::new(v, false);
                    row
                })
                .collect()
        };
        let cookies = if entry.cookies.is_empty() {
            Vec::new()
        } else {
            entry
                .cookies
                .iter()
                .map(|(k, v)| {
                    let mut row = HeaderRow::new();
                    row.key = Editor::new(k, false);
                    row.value = Editor::new(v, false);
                    row
                })
                .collect()
        };
        let form_fields = if entry.form_fields.is_empty() {
            Vec::new()
        } else {
            entry
                .form_fields
                .iter()
                .map(|f| {
                    let mut row = FormRow::new();
                    row.key = Editor::new(&f.key, false);
                    row.value = Editor::new(&f.value, false);
                    row.ctype = Editor::new(f.content_type.as_deref().unwrap_or(""), false);
                    row.base64_prefix =
                        Editor::new(f.base64_prefix.as_deref().unwrap_or(""), false);
                    row.kind = f.kind;
                    row
                })
                .collect()
        };
        let asserts = entry
            .asserts
            .iter()
            .map(|a| {
                let mut row = AssertRow::new();
                row.expr = Editor::new(a, false);
                row
            })
            .collect();
        let captures = entry
            .captures
            .iter()
            .map(|(n, e)| {
                let mut row = CaptureRow::new();
                row.name = Editor::new(n, false);
                row.expr = Editor::new(e, false);
                row
            })
            .collect();
        Self {
            name: Editor::new(&entry.title, false),
            url: Editor::new(&entry.url, false),
            headers,
            cookies,
            form_fields,
            body: Editor::new(entry.body.as_deref().unwrap_or(""), true),
            asserts,
            captures,
            method_idx,
            focus: NewField::Name,
            target_idx: ci.min(target_names.len().saturating_sub(1)),
            target_names,
            base_url,
            suggest_hi: None,
            suggest_hidden: false,
            kind_dropdown_hidden: false,
            ctype_dropdown_hidden: false,
            ctype_hi: None,
            desc_visible: std::cell::Cell::new(true),
            key_cell_rect: std::cell::Cell::new(None),
            kind_cell_rect: std::cell::Cell::new(None),
            ctype_cell_rect: std::cell::Cell::new(None),
            cookie_desc_visible: std::cell::Cell::new(true),
            form_desc_visible: std::cell::Cell::new(true),
            form_ctype_visible: std::cell::Cell::new(true),
            form_prefix_visible: std::cell::Cell::new(true),
            header_scroll: std::cell::Cell::new(0),
            cookie_scroll: std::cell::Cell::new(0),
            form_scroll: std::cell::Cell::new(0),
            assert_scroll: std::cell::Cell::new(0),
            capture_scroll: std::cell::Cell::new(0),
            file_root,
            editing: Some((ci, ei)),
            view_tab: WizardTab::All,
            tab_order: WizardTab::ALL.to_vec(),
        }
    }

    /// Display name of the currently selected target collection.
    pub(crate) fn target_name(&self) -> &str {
        self.target_names
            .get(self.target_idx)
            .map(String::as_str)
            .unwrap_or("Request")
    }

    pub(crate) fn method(&self) -> &'static str {
        METHODS[self.method_idx.min(METHODS.len() - 1)]
    }

    /// The header-name matches for the focused Key cell, ignoring whether the
    /// dropdown has been dismissed via [`Self::suggest_hidden`]. Used both by
    /// [`Self::key_dropdown`] and to decide whether Enter should be able to
    /// reveal a dropdown that arrow-key navigation auto-hid.
    fn key_suggestions(&self) -> Option<(usize, Vec<&'static str>)> {
        let NewField::Header(i, HdrCol::Key) = self.focus else {
            return None;
        };
        let text = self.headers.get(i)?.key.text();
        let sugs = filter_headers(&text);
        let single_exact = sugs.len() == 1 && sugs[0].eq_ignore_ascii_case(text.trim());
        (!sugs.is_empty() && !single_exact).then_some((i, sugs))
    }

    /// The suggestion dropdown for the focused Key cell, if it should be shown:
    /// returns `(row_index, filtered_header_names)`. Hidden when dismissed, when
    /// the filter is empty, or when the only match already equals the text.
    pub(crate) fn key_dropdown(&self) -> Option<(usize, Vec<&'static str>)> {
        if self.suggest_hidden {
            return None;
        }
        self.key_suggestions()
    }

    /// Whether pressing Enter on the focused Key cell should reveal a
    /// currently-hidden dropdown (rather than advance focus): true only when
    /// the cell has matches to show but the dropdown isn't already open.
    pub(crate) fn key_dropdown_revealable(&self) -> bool {
        self.suggest_hidden && self.key_suggestions().is_some()
    }

    /// Fill the focused Key cell with header `name` and close the dropdown.
    pub(crate) fn accept_suggestion(&mut self, name: &str) {
        if let NewField::Header(i, HdrCol::Key) = self.focus
            && let Some(row) = self.headers.get_mut(i)
        {
            row.key = Editor::new(name, false);
        }
        self.suggest_hi = None;
        self.suggest_hidden = true;
    }

    /// Whether the Form Kind (Text/File) dropdown should currently be shown:
    /// only when a Kind cell is focused and the user hasn't dismissed it with
    /// Esc since arriving there.
    pub(crate) fn kind_dropdown_open(&self) -> bool {
        matches!(self.focus, NewField::FormField(_, FormCol::Kind)) && !self.kind_dropdown_hidden
    }

    /// Whether pressing Enter on the focused Kind cell should reveal a
    /// currently-hidden dropdown (rather than advance focus): true whenever
    /// the dropdown is hidden — a Kind cell always has Text/File picked (it
    /// defaults to Text on a fresh row), so it never auto-opens on its own.
    pub(crate) fn kind_dropdown_revealable(&self) -> bool {
        let NewField::FormField(_, FormCol::Kind) = self.focus else {
            return false;
        };
        self.kind_dropdown_hidden
    }

    /// Whether the content-type dropdown should currently be shown: only
    /// when a `File`-kind Form row's Content-Type cell is focused and the
    /// user hasn't dismissed it with Esc since arriving there.
    pub(crate) fn ctype_dropdown_open(&self) -> bool {
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return false;
        };
        !self.ctype_dropdown_hidden
            && self.form_fields.get(i).map(|r| r.kind) == Some(FormFieldKind::File)
    }

    /// Whether pressing Enter on the focused Content-Type cell should reveal
    /// a currently-hidden dropdown: true only when the cell already has an
    /// override typed in and the dropdown is hidden (a fresh, empty cell
    /// already shows the dropdown automatically).
    pub(crate) fn ctype_dropdown_revealable(&self) -> bool {
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return false;
        };
        self.ctype_dropdown_hidden
            && self
                .form_fields
                .get(i)
                .is_some_and(|r| r.kind == FormFieldKind::File && !r.ctype.text().is_empty())
    }

    /// The MIME options shown by the content-type dropdown for the focused
    /// Content-Type cell, filtered down to those matching whatever the user
    /// has typed so far (see [`filter_content_types`]) — the same
    /// filter-as-you-type behaviour as the Key dropdown in Headers. Empty
    /// when no Content-Type cell is focused.
    pub(crate) fn ctype_filtered_options(&self) -> Vec<&'static str> {
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return Vec::new();
        };
        let query = self
            .form_fields
            .get(i)
            .map(|r| r.ctype.text())
            .unwrap_or_default();
        filter_content_types(&query)
    }

    /// Whether the "Auto" entry should currently appear in the content-type
    /// dropdown: always while the override is empty (Auto is the implicit
    /// default then), otherwise it's filtered exactly like any other
    /// option — only shown while what's been typed still matches its own
    /// (localized) label.
    pub(crate) fn ctype_auto_visible(&self, s: &Strings) -> bool {
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return false;
        };
        let query = self
            .form_fields
            .get(i)
            .map(|r| r.ctype.text())
            .unwrap_or_default();
        let q = query.trim();
        q.is_empty()
            || s.content_type_auto
                .to_ascii_lowercase()
                .contains(&q.to_ascii_lowercase())
    }

    /// The content-type dropdown's entries for the focused cell, in display
    /// order: `None` for "Auto" (clears the override), included only while
    /// it's still a match for what's been typed (see
    /// [`Self::ctype_auto_visible`]), followed by the filtered MIME options.
    /// Shared by what's drawn on screen and by index-based
    /// selection/accept, so they can never disagree about what's at a given
    /// row.
    pub(crate) fn ctype_dropdown_entries(&self, s: &Strings) -> Vec<Option<&'static str>> {
        let mut entries = Vec::new();
        if self.ctype_auto_visible(s) {
            entries.push(None);
        }
        entries.extend(self.ctype_filtered_options().into_iter().map(Some));
        entries
    }

    /// Number of selectable rows in the content-type dropdown for the
    /// focused cell (including "Auto" when it's still visible — see
    /// [`Self::ctype_auto_visible`]). Used to clamp Down-arrow navigation to
    /// the currently filtered list, not the full unfiltered one.
    pub(crate) fn ctype_option_count(&self, s: &Strings) -> usize {
        self.ctype_dropdown_entries(s).len()
    }

    /// Apply the highlighted content-type dropdown option (see
    /// [`Self::ctype_dropdown_entries`]; "Auto" clears the override so Hurl
    /// infers the type itself) to the focused Form row's Content-Type cell,
    /// then dismiss the dropdown.
    pub(crate) fn accept_content_type(&mut self, s: &Strings) {
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return;
        };
        let Some(hi) = self.ctype_hi else { return };
        let entries = self.ctype_dropdown_entries(s);
        let text = entries.get(hi).copied().flatten().unwrap_or("").to_string();
        if let Some(row) = self.form_fields.get_mut(i) {
            row.ctype = Editor::new(&text, false);
        }
        self.ctype_hi = None;
        self.ctype_dropdown_hidden = true;
    }

    /// The content-type dropdown's currently highlighted index: the user's
    /// explicit arrow-key selection (`ctype_hi`) if there is one, otherwise
    /// whichever entry matches the focused row's current override text
    /// (`Some(0)` for "Auto" when it's empty, `None` if the text doesn't
    /// match any listed option). Arrow-key navigation starts from this
    /// implicit position instead of always restarting at "Auto", which is
    /// what made the first Down press look like it did nothing.
    pub(crate) fn ctype_selected_index(&self, s: &Strings) -> Option<usize> {
        if self.ctype_hi.is_some() {
            return self.ctype_hi;
        }
        let NewField::FormField(i, FormCol::Ctype) = self.focus else {
            return None;
        };
        let current = self.form_fields.get(i)?.ctype.text();
        let entries = self.ctype_dropdown_entries(s);
        if current.trim().is_empty() {
            Some(0)
        } else {
            entries.iter().position(|e| *e == Some(current.trim()))
        }
    }

    /// The text editor for the focused field, if it is a text field.
    pub(crate) fn active_editor(&mut self) -> Option<&mut Editor> {
        match self.focus {
            NewField::Name => Some(&mut self.name),
            NewField::Url => Some(&mut self.url),
            NewField::Body => Some(&mut self.body),
            NewField::Header(i, col) => self.headers.get_mut(i).and_then(|r| r.cell_mut(col)),
            NewField::Cookie(i, col) => self.cookies.get_mut(i).and_then(|r| r.cell_mut(col)),
            NewField::FormField(i, col) => {
                self.form_fields.get_mut(i).and_then(|r| r.cell_mut(col))
            }
            NewField::Assert(i) => self.asserts.get_mut(i).map(|r| &mut r.expr),
            NewField::Capture(i, col) => self.captures.get_mut(i).map(|r| r.cell_mut(col)),
            NewField::Method
            | NewField::Target
            | NewField::AddHeader
            | NewField::AddCookie
            | NewField::AddFormField
            | NewField::AddAssert
            | NewField::AddCapture => None,
        }
    }

    /// True when every `[Asserts]` row is blank — the section is then skipped
    /// when tabbing between Body and `[Captures]`. Vacuously true when the
    /// section has no rows at all (the default, now that Asserts/Captures no
    /// longer seed a placeholder blank row).
    pub(crate) fn asserts_blank(&self) -> bool {
        self.asserts.iter().all(AssertRow::is_blank)
    }

    /// True when every `[Captures]` row is blank — the section is then skipped
    /// when tabbing between `[Asserts]` and Name.
    pub(crate) fn captures_blank(&self) -> bool {
        self.captures.iter().all(CaptureRow::is_blank)
    }

    /// The field that represents "arriving at the `[Asserts]` section for the
    /// first time": its first row when one exists, or the "+ Add Assert"
    /// row when the section is empty (there's no default blank row to land
    /// on any more).
    pub(crate) fn assert_entry(&self) -> NewField {
        if self.asserts.is_empty() {
            NewField::AddAssert
        } else {
            NewField::Assert(0)
        }
    }

    /// Like [`Self::assert_entry`], but for `[Captures]`.
    pub(crate) fn capture_entry(&self) -> NewField {
        if self.captures.is_empty() {
            NewField::AddCapture
        } else {
            NewField::Capture(0, CapCol::Name)
        }
    }

    /// The column to the left of `col` within a capture row, if any.
    pub(crate) fn prev_cap_col(&self, col: CapCol) -> Option<CapCol> {
        match col {
            CapCol::Expr => Some(CapCol::Name),
            CapCol::Name => None,
        }
    }

    /// The column to the right of `col` within a capture row, if any.
    pub(crate) fn next_cap_col(&self, col: CapCol) -> Option<CapCol> {
        match col {
            CapCol::Name => Some(CapCol::Expr),
            CapCol::Expr => None,
        }
    }

    /// True when every header row is blank — the whole header section can then
    /// be skipped when tabbing between URL and Cookies.
    pub(crate) fn headers_blank(&self) -> bool {
        self.headers.iter().all(HeaderRow::is_blank)
    }

    /// Delete the currently focused Header/Cookie/Form/Assert/Capture row,
    /// moving focus to the row that slides into its place (same column), or to
    /// the section's "+ Add …" row when it becomes empty. A no-op when focus
    /// isn't on a deletable row.
    pub(crate) fn delete_focused_row(&mut self) {
        self.focus = match self.focus {
            NewField::Header(i, _) if i < self.headers.len() => {
                self.headers.remove(i);
                if self.headers.is_empty() {
                    NewField::AddHeader
                } else {
                    NewField::Header(i.min(self.headers.len() - 1), HdrCol::Key)
                }
            }
            NewField::Cookie(i, _) if i < self.cookies.len() => {
                self.cookies.remove(i);
                if self.cookies.is_empty() {
                    NewField::AddCookie
                } else {
                    NewField::Cookie(i.min(self.cookies.len() - 1), HdrCol::Key)
                }
            }
            NewField::FormField(i, _) if i < self.form_fields.len() => {
                self.form_fields.remove(i);
                if self.form_fields.is_empty() {
                    NewField::AddFormField
                } else {
                    NewField::FormField(i.min(self.form_fields.len() - 1), FormCol::Key)
                }
            }
            NewField::Assert(i) if i < self.asserts.len() => {
                self.asserts.remove(i);
                if self.asserts.is_empty() {
                    NewField::AddAssert
                } else {
                    NewField::Assert(i.min(self.asserts.len() - 1))
                }
            }
            NewField::Capture(i, _) if i < self.captures.len() => {
                self.captures.remove(i);
                if self.captures.is_empty() {
                    NewField::AddCapture
                } else {
                    NewField::Capture(i.min(self.captures.len() - 1), CapCol::Name)
                }
            }
            other => other,
        };
    }

    /// Toggle the enabled flag of the focused Header/Cookie/Form row in place,
    /// leaving focus untouched so it can be pressed mid-edit. A no-op on fields
    /// without an enabled flag (everything else, including Asserts/Captures).
    pub(crate) fn toggle_focused_enabled(&mut self) {
        let row = match self.focus {
            NewField::Header(i, _) => self.headers.get_mut(i).map(|r| &mut r.enabled),
            NewField::Cookie(i, _) => self.cookies.get_mut(i).map(|r| &mut r.enabled),
            NewField::FormField(i, _) => self.form_fields.get_mut(i).map(|r| &mut r.enabled),
            _ => None,
        };
        if let Some(enabled) = row {
            *enabled = !*enabled;
        }
    }

    /// True when every cookie row is blank — the section is then skipped when
    /// tabbing between Headers and Form.
    pub(crate) fn cookies_blank(&self) -> bool {
        self.cookies.iter().all(HeaderRow::is_blank)
    }

    /// True when every form-field row is blank — the section is then skipped
    /// when tabbing between Cookies and Body.
    pub(crate) fn form_fields_blank(&self) -> bool {
        self.form_fields.iter().all(FormRow::is_blank)
    }

    /// The field that represents "arriving at the Headers section": its
    /// first row when one exists, or the "+ Add Header" row when the
    /// section is empty (there's no default blank row any more, matching
    /// Asserts/Captures' `assert_entry`/`capture_entry`).
    pub(crate) fn header_entry(&self) -> NewField {
        if self.headers.is_empty() {
            NewField::AddHeader
        } else {
            NewField::Header(0, HdrCol::Key)
        }
    }

    /// Like [`Self::header_entry`], but for Cookies.
    pub(crate) fn cookie_entry(&self) -> NewField {
        if self.cookies.is_empty() {
            NewField::AddCookie
        } else {
            NewField::Cookie(0, HdrCol::Key)
        }
    }

    /// Like [`Self::header_entry`], but for the Form/Multipart section.
    pub(crate) fn form_entry(&self) -> NewField {
        if self.form_fields.is_empty() {
            NewField::AddFormField
        } else {
            NewField::FormField(0, FormCol::Key)
        }
    }

    /// Where Up-arrow lands when leaving the first Cookie row upward: the
    /// last Header row if any exist, otherwise the "+ Add Header" row —
    /// arrow-key row navigation now stops at every section exactly like
    /// Down does (which always lands on a section's "+ Add …" row before
    /// moving on), so an empty Headers section is never skipped over.
    pub(crate) fn up_into_headers(&self) -> NewField {
        if self.headers.is_empty() {
            NewField::AddHeader
        } else {
            NewField::Header(self.headers.len() - 1, HdrCol::Key)
        }
    }

    /// Like [`Self::up_into_headers`], but for leaving the first Form-field
    /// row upward into Cookies: lands on the last Cookie row, or "+ Add
    /// Cookie" when Cookies is empty — never skips straight past it into
    /// Headers, mirroring Down's own one-section-at-a-time behaviour.
    pub(crate) fn up_into_cookies(&self) -> NewField {
        if self.cookies.is_empty() {
            NewField::AddCookie
        } else {
            NewField::Cookie(self.cookies.len() - 1, HdrCol::Key)
        }
    }

    /// Like [`Self::up_into_headers`], but for leaving the first Capture row
    /// upward into Asserts: lands on the last Assert row, or "+ Add Assert"
    /// when Asserts is empty — never skips straight past it into Body.
    pub(crate) fn up_into_asserts(&self) -> NewField {
        if self.asserts.is_empty() {
            NewField::AddAssert
        } else {
            NewField::Assert(self.asserts.len() - 1)
        }
    }

    /// All columns of a `Enabled/Key/Value/Desc`-shaped row, in left-to-right
    /// visual order, for arrow-key navigation. The Enabled checkbox comes
    /// first, matching its position as the leftmost column on screen — so
    /// `Left` from Key reaches it directly instead of requiring a wrap-around
    /// trip through every other column. Description is omitted when the
    /// column is too narrow to be shown.
    fn hdr_row_cells(desc_visible: bool) -> Vec<HdrCol> {
        if desc_visible {
            vec![HdrCol::Enabled, HdrCol::Key, HdrCol::Value, HdrCol::Desc]
        } else {
            vec![HdrCol::Enabled, HdrCol::Key, HdrCol::Value]
        }
    }

    /// Columns visited by Tab / Shift+Tab within a `Key/Value/Desc/Enabled`
    /// row. The Enabled checkbox is intentionally excluded — it is reached
    /// with the arrow keys or by pressing Ctrl+E. A brand new row always
    /// starts focus on Key regardless of this order (set explicitly when the
    /// row is created), not by tabbing from the first entry here.
    fn hdr_tab_cells(desc_visible: bool) -> Vec<HdrCol> {
        if desc_visible {
            vec![HdrCol::Key, HdrCol::Value, HdrCol::Desc]
        } else {
            vec![HdrCol::Key, HdrCol::Value]
        }
    }

    /// All columns of a header row, for arrow-key navigation. See
    /// [`Self::hdr_row_cells`].
    pub(crate) fn row_cells(&self) -> Vec<HdrCol> {
        Self::hdr_row_cells(self.desc_visible.get())
    }

    /// Columns visited by Tab / Shift+Tab within a header row.
    pub(crate) fn tab_cells(&self) -> Vec<HdrCol> {
        Self::hdr_tab_cells(self.desc_visible.get())
    }

    /// The column to the left of `col` within a header row, if any.
    pub(crate) fn prev_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        idx.checked_sub(1).map(|p| cells[p])
    }

    /// The column to the right of `col` within a header row, if any.
    pub(crate) fn next_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        cells.get(idx + 1).copied()
    }

    /// All columns of a cookie row, for arrow-key navigation (a Cookie row has
    /// the same shape as a header row, with its own Description visibility).
    pub(crate) fn cookie_row_cells(&self) -> Vec<HdrCol> {
        Self::hdr_row_cells(self.cookie_desc_visible.get())
    }

    /// Columns visited by Tab / Shift+Tab within a cookie row.
    pub(crate) fn cookie_tab_cells(&self) -> Vec<HdrCol> {
        Self::hdr_tab_cells(self.cookie_desc_visible.get())
    }

    /// The column to the left of `col` within a cookie row, if any.
    pub(crate) fn prev_cookie_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.cookie_row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        idx.checked_sub(1).map(|p| cells[p])
    }

    /// The column to the right of `col` within a cookie row, if any.
    pub(crate) fn next_cookie_col(&self, col: HdrCol) -> Option<HdrCol> {
        let cells = self.cookie_row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        cells.get(idx + 1).copied()
    }

    /// All columns of a Form row, in left-to-right visual order, for
    /// arrow-key navigation. The Enabled checkbox comes first, matching its
    /// position as the leftmost column on screen — so `Left` from Key reaches
    /// it directly instead of requiring a wrap-around trip through every
    /// other column. Kind comes before Value (rather than after) so that
    /// filling a row top-to-bottom naturally lands on Kind — Text vs. File —
    /// before Value, instead of after (this also matters for File rows,
    /// whose Value is a file path best chosen once Kind is already known).
    /// Content-Type/Description are omitted when their column is too narrow
    /// to be shown.
    pub(crate) fn form_row_cells(&self) -> Vec<FormCol> {
        let mut cols = vec![
            FormCol::Enabled,
            FormCol::Key,
            FormCol::Kind,
            FormCol::Value,
        ];
        if self.form_ctype_visible.get() {
            cols.push(FormCol::Ctype);
        }
        if self.form_prefix_visible.get() {
            cols.push(FormCol::Prefix);
        }
        if self.form_desc_visible.get() {
            cols.push(FormCol::Desc);
        }
        cols
    }

    /// Columns visited by Tab / Shift+Tab within a Form row. The Enabled
    /// checkbox is excluded (arrow keys / Ctrl+E); Kind is included since it
    /// isn't a text field but is still Tab-reachable (its own dropdown opens
    /// on focus; Up/Down flip Text/File while Left/Right hop columns). A
    /// brand new row always starts focus on Key regardless of this order
    /// (set explicitly when the row is created), not by tabbing from the
    /// first entry here.
    pub(crate) fn form_tab_cells(&self) -> Vec<FormCol> {
        let mut cols = vec![FormCol::Key, FormCol::Kind, FormCol::Value];
        if self.form_ctype_visible.get() {
            cols.push(FormCol::Ctype);
        }
        if self.form_prefix_visible.get() {
            cols.push(FormCol::Prefix);
        }
        if self.form_desc_visible.get() {
            cols.push(FormCol::Desc);
        }
        cols
    }

    /// The column to the left of `col` within a Form row, if any (arrow-key
    /// navigation — includes the Enabled checkbox, unlike the Tab order).
    pub(crate) fn prev_form_col(&self, col: FormCol) -> Option<FormCol> {
        let cells = self.form_row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        idx.checked_sub(1).map(|p| cells[p])
    }

    /// The column to the right of `col` within a Form row, if any (arrow-key
    /// navigation).
    pub(crate) fn next_form_col(&self, col: FormCol) -> Option<FormCol> {
        let cells = self.form_row_cells();
        let idx = cells.iter().position(|c| *c == col)?;
        cells.get(idx + 1).copied()
    }

    pub(crate) fn focus_next(&mut self, forward: bool) {
        self.focus = if forward {
            self.next_forward()
        } else {
            self.next_backward()
        };
    }

    /// A total ordering over every focusable field, used to walk the Tab
    /// ring. The tuple is `(section, row, column)`; `Enabled` sorts after a
    /// row's real Tab columns (so Tab off it lands on the next row) and each
    /// "+ Add …" row sorts after all of its section's rows.
    fn field_key(f: NewField) -> (u8, usize, u8) {
        fn hdr(c: HdrCol) -> u8 {
            match c {
                HdrCol::Key => 0,
                HdrCol::Value => 1,
                HdrCol::Desc => 2,
                HdrCol::Enabled => 3,
            }
        }
        fn form(c: FormCol) -> u8 {
            match c {
                FormCol::Key => 0,
                FormCol::Kind => 1,
                FormCol::Value => 2,
                FormCol::Ctype => 3,
                FormCol::Prefix => 4,
                FormCol::Desc => 5,
                FormCol::Enabled => 6,
            }
        }
        match f {
            NewField::Name => (0, 0, 0),
            NewField::Target => (1, 0, 0),
            NewField::Method => (2, 0, 0),
            NewField::Url => (3, 0, 0),
            NewField::Header(i, c) => (4, i, hdr(c)),
            NewField::AddHeader => (4, usize::MAX, 0),
            NewField::Cookie(i, c) => (5, i, hdr(c)),
            NewField::AddCookie => (5, usize::MAX, 0),
            NewField::FormField(i, c) => (6, i, form(c)),
            NewField::AddFormField => (6, usize::MAX, 0),
            NewField::Body => (7, 0, 0),
            NewField::Assert(i) => (8, i, 0),
            NewField::AddAssert => (8, usize::MAX, 0),
            NewField::Capture(i, c) => (9, i, if c == CapCol::Name { 0 } else { 1 }),
            NewField::AddCapture => (9, usize::MAX, 0),
        }
    }

    /// The ordered list of fields Tab / Shift+Tab visits, in visual order,
    /// given the current form state. A section that is entirely blank
    /// contributes a single "entry" stop (its first row, or its "+ Add …"
    /// row when it has none) and is otherwise skipped; a non-blank section
    /// contributes each row's Tab columns followed by its Add row. Because
    /// `next_forward`/`next_backward` are just steps along this one list,
    /// they can never disagree about ordering or empty-section skipping.
    fn tab_stops(&self) -> Vec<NewField> {
        let mut v = vec![
            NewField::Name,
            NewField::Target,
            NewField::Method,
            NewField::Url,
        ];
        if self.headers_blank() {
            v.push(self.header_entry());
        } else {
            let cells = self.tab_cells();
            for i in 0..self.headers.len() {
                v.extend(cells.iter().map(|&c| NewField::Header(i, c)));
            }
            v.push(NewField::AddHeader);
        }
        if self.cookies_blank() {
            v.push(self.cookie_entry());
        } else {
            let cells = self.cookie_tab_cells();
            for i in 0..self.cookies.len() {
                v.extend(cells.iter().map(|&c| NewField::Cookie(i, c)));
            }
            v.push(NewField::AddCookie);
        }
        if self.form_fields_blank() {
            v.push(self.form_entry());
        } else {
            let cells = self.form_tab_cells();
            for i in 0..self.form_fields.len() {
                v.extend(cells.iter().map(|&c| NewField::FormField(i, c)));
            }
            v.push(NewField::AddFormField);
        }
        v.push(NewField::Body);
        if self.asserts_blank() {
            v.push(self.assert_entry());
        } else {
            v.extend((0..self.asserts.len()).map(NewField::Assert));
            v.push(NewField::AddAssert);
        }
        if self.captures_blank() {
            v.push(self.capture_entry());
        } else {
            for i in 0..self.captures.len() {
                v.push(NewField::Capture(i, CapCol::Name));
                v.push(NewField::Capture(i, CapCol::Expr));
            }
            v.push(NewField::AddCapture);
        }
        v
    }

    /// Step one stop forward or backward around the Tab ring, wrapping at
    /// the ends. Works even when `self.focus` isn't itself a stop (e.g. an
    /// `Enabled` checkbox reached with the arrow keys): its ordering key
    /// still places it between the appropriate stops.
    fn step(&self, forward: bool) -> NewField {
        let stops = self.tab_stops();
        let here = Self::field_key(self.focus);
        if forward {
            stops
                .iter()
                .copied()
                .find(|&s| Self::field_key(s) > here)
                .unwrap_or(stops[0])
        } else {
            stops
                .iter()
                .rev()
                .copied()
                .find(|&s| Self::field_key(s) < here)
                .unwrap_or(stops[stops.len() - 1])
        }
    }

    pub(crate) fn next_forward(&self) -> NewField {
        self.step(true)
    }

    pub(crate) fn next_backward(&self) -> NewField {
        self.step(false)
    }

    /// Ctrl+Down: jump straight to the first field of the next section
    /// (Headers → Cookies → Form → Body → Asserts → Captures, wrapping to
    /// Name), skipping the rest of the current section's rows/columns. Meta
    /// fields (Name/Target/Method/Url) fall back to a normal Tab step, since
    /// each is already a single-field "section".
    pub(crate) fn jump_forward(&self) -> NewField {
        match self.focus {
            NewField::Name | NewField::Target | NewField::Method | NewField::Url => {
                self.next_forward()
            }
            NewField::Header(..) | NewField::AddHeader => self.cookie_entry(),
            NewField::Cookie(..) | NewField::AddCookie => self.form_entry(),
            NewField::FormField(..) | NewField::AddFormField => NewField::Body,
            NewField::Body => self.assert_entry(),
            NewField::Assert(..) | NewField::AddAssert => self.capture_entry(),
            NewField::Capture(..) | NewField::AddCapture => NewField::Name,
        }
    }

    /// Ctrl+Up: jump straight to the first field of the previous section.
    /// Mirrors `jump_forward()`'s fixed section-to-section mapping (no
    /// blank-section skipping — that's Tab's job, not Ctrl+Arrow's).
    pub(crate) fn jump_backward(&self) -> NewField {
        match self.focus {
            // Wrapping backward past the first field lands on the last section,
            // matching jump_forward's `Capture -> Name` wrap.
            NewField::Name => self.capture_entry(),
            NewField::Target | NewField::Method | NewField::Url => self.next_backward(),
            NewField::Header(..) | NewField::AddHeader => NewField::Url,
            NewField::Cookie(..) | NewField::AddCookie => self.header_entry(),
            NewField::FormField(..) | NewField::AddFormField => self.cookie_entry(),
            NewField::Body => self.form_entry(),
            NewField::Assert(..) | NewField::AddAssert => NewField::Body,
            NewField::Capture(..) | NewField::AddCapture => self.assert_entry(),
        }
    }

    /// Like [`WizardTab::first_field`], but empty-aware for Headers/
    /// Cookies/Form/Asserts/Captures: since none of those sections seed a
    /// default blank row any more, their "first field" is the "+ Add …" row
    /// whenever they have none.
    pub(crate) fn first_field_of(&self, tab: WizardTab) -> NewField {
        match tab {
            WizardTab::Headers => self.header_entry(),
            WizardTab::Cookies => self.cookie_entry(),
            WizardTab::Form => self.form_entry(),
            WizardTab::Asserts => self.assert_entry(),
            WizardTab::Captures => self.capture_entry(),
            other => other.first_field(),
        }
    }

    /// Whether the currently focused field is a free-text editor cell (as
    /// opposed to a selector, checkbox, dropdown, or "+ Add …" row). Used to
    /// decide whether `[` / `]` should type into the field or act as
    /// tab-cycling shortcuts.
    pub(crate) fn focus_is_text_entry(&self) -> bool {
        match self.focus {
            NewField::Name | NewField::Url | NewField::Body => true,
            NewField::Assert(_) | NewField::Capture(..) => true,
            NewField::Header(_, col) | NewField::Cookie(_, col) => {
                matches!(col, HdrCol::Key | HdrCol::Value | HdrCol::Desc)
            }
            NewField::FormField(_, col) => matches!(
                col,
                FormCol::Key | FormCol::Value | FormCol::Ctype | FormCol::Prefix | FormCol::Desc
            ),
            NewField::Method
            | NewField::Target
            | NewField::AddHeader
            | NewField::AddCookie
            | NewField::AddFormField
            | NewField::AddAssert
            | NewField::AddCapture => false,
        }
    }

    /// PageUp/PageDown: cycle the active section-view tab, wrapping around
    /// `self.tab_order` (which the user may have reordered). When landing on
    /// a section tab (anything but `All`), focus jumps to that section's
    /// first field — for `Body` this means focus lands on the editor
    /// itself, which is exactly what puts it into edit mode.
    pub(crate) fn cycle_view_tab(&mut self, forward: bool) {
        let len = self.tab_order.len();
        if len == 0 {
            return;
        }
        let idx = self
            .tab_order
            .iter()
            .position(|t| *t == self.view_tab)
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % len
        } else {
            (idx + len - 1) % len
        };
        self.view_tab = self.tab_order[next];
        if self.view_tab != WizardTab::All {
            self.focus = self.first_field_of(self.view_tab);
        }
    }

    /// Ctrl+Shift+Left/Right: reorder the active tab within `tab_order`,
    /// mirroring how collection tabs are reordered. `All` is always pinned
    /// at index 0 — it can never move, and nothing can move before it.
    pub(crate) fn move_view_tab(&mut self, forward: bool) {
        let len = self.tab_order.len();
        if len < 2 {
            return;
        }
        let idx = match self.tab_order.iter().position(|t| *t == self.view_tab) {
            Some(0) => return, // `All` is pinned first; never movable.
            Some(i) => i,
            None => return,
        };
        let target = if forward {
            idx + 1
        } else {
            idx.wrapping_sub(1)
        };
        if target == 0 || target >= len {
            return;
        }
        self.tab_order.swap(idx, target);
    }
}

/// Height (in rows) to give a section's table in the combined "All" layout:
/// it grows with the row count up to a cap of 5 visible data rows, then
/// stays fixed and lets `windowed_table_rows` take over with a scrollbar,
/// instead of splitting surplus space evenly across every section.
/// `header_h` is 1 for tables with a column-header line (Headers/Cookies/
/// Form/Captures) and 0 for Asserts (no header row); the trailing `+1` is
/// the always-visible "+ Add ..." row.
pub(crate) fn section_height(header_h: u16, row_count: usize) -> u16 {
    header_h + (row_count.min(5) as u16) + 1
}

pub(crate) fn draw_new_request(
    f: &mut Frame,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
    enhanced: bool,
) {
    let w = (f.area().width * 7 / 10).max(52);
    let h = 36u16.min(f.area().height);
    let area = centered_rect(w, h, f.area());
    f.render_widget(Clear, area);
    // Drop the Ctrl+Enter submit shortcut from the hint on terminals that can't
    // report it distinctly (F2 stays as the universal trigger).
    let hint_str = if form.editing.is_some() {
        s.edit_request_hint
    } else {
        s.new_request_hint
    };
    let mut hint = if enhanced {
        hint_str.to_string()
    } else {
        hint_str.replace(&format!("{}/F2", s.ctrl_enter_key), "F2")
    };
    // Contextual addition: only shown while a `File`- or `Base64 File`-kind
    // Form row's Value cell is focused (both pick a file), so the
    // always-visible hint bar stays short otherwise.
    if let NewField::FormField(i, FormCol::Value) = form.focus
        && form
            .form_fields
            .get(i)
            .map(|r| r.kind)
            .is_some_and(|v| v.is_multipart())
    {
        hint = format!("{hint} · {}", s.hint_pick_file);
    }
    // Contextual addition: only shown while focus is on an existing
    // Header/Cookie/Form row (the row kinds that actually have an enabled
    // checkbox — Asserts/Captures don't), so users can discover the Ctrl+E
    // toggle-enabled shortcut without it cluttering the hint bar elsewhere.
    let on_toggleable_row = matches!(
        form.focus,
        NewField::Header(i, _) if i < form.headers.len()
    ) || matches!(
        form.focus,
        NewField::Cookie(i, _) if i < form.cookies.len()
    ) || matches!(
        form.focus,
        NewField::FormField(i, _) if i < form.form_fields.len()
    );
    if on_toggleable_row {
        hint = format!("{hint} · {}", s.hint_toggle_enabled);
    }
    // Contextual addition: only shown while focus is on an existing
    // Header/Cookie/Form/Assert/Capture row, so users can discover the
    // Ctrl+D row-delete shortcut without it cluttering the hint bar on
    // every other field.
    let on_deletable_row = on_toggleable_row
        || matches!(
            form.focus,
            NewField::Assert(i) if i < form.asserts.len()
        )
        || matches!(
            form.focus,
            NewField::Capture(i, _) if i < form.captures.len()
        );
    if on_deletable_row {
        hint = format!("{hint} · {}", s.hint_delete_row);
    }
    let title_text = if form.editing.is_some() {
        s.edit_request
    } else {
        s.new_request
    };
    let title = format!("{}   ({})", title_text, hint);
    let block = panel(title, true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // An existing entry stays in its original collection, so the Target row is
    // hidden entirely (zero height) while editing.
    let target_h = if form.editing.is_some() { 0 } else { 1 };
    let rows = Layout::vertical([
        Constraint::Length(1),        // name
        Constraint::Length(target_h), // target collection
        Constraint::Length(1),        // method
        Constraint::Length(1),        // url
        Constraint::Length(1),        // tab bar
        Constraint::Min(0),           // section area (All: every section stacked; else: just one)
    ])
    .split(inner);

    new_req_text_row(
        f,
        rows[0],
        s.field_name,
        &form.name,
        form.focus == NewField::Name,
        th,
    );

    if form.editing.is_none() {
        // Target collection row (cycler, no cursor).
        let tcols = Layout::horizontal([Constraint::Length(10), Constraint::Min(1)]).split(rows[1]);
        let t_focused = form.focus == NewField::Target;
        f.render_widget(
            Paragraph::new(Span::styled(
                s.field_target.to_string(),
                Style::default()
                    .fg(if t_focused { th.accent } else { th.dim })
                    .add_modifier(Modifier::BOLD),
            )),
            tcols[0],
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("< {} >", form.target_name()),
                Style::default().fg(th.text).add_modifier(Modifier::BOLD),
            )),
            tcols[1],
        );
    }

    // Method row (cycler, no cursor).
    let cols = Layout::horizontal([Constraint::Length(10), Constraint::Min(1)]).split(rows[2]);
    let m_focused = form.focus == NewField::Method;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_method.to_string(),
            Style::default()
                .fg(if m_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        cols[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("< {} >", form.method()),
            Style::default()
                .fg(method_color(form.method()))
                .add_modifier(Modifier::BOLD),
        )),
        cols[1],
    );

    draw_url_row(f, rows[3], form, s, th);
    draw_wizard_tab_bar(f, rows[4], form.view_tab, &form.tab_order, s, th);

    if form.view_tab == WizardTab::All {
        let sub = Layout::vertical([
            Constraint::Length(1),                                         // headers label
            Constraint::Length(section_height(1, form.headers.len())),     // headers table
            Constraint::Length(1),                                         // cookies label
            Constraint::Length(section_height(1, form.cookies.len())),     // cookies table
            Constraint::Length(1),                                         // form label
            Constraint::Length(section_height(1, form.form_fields.len())), // form table
            Constraint::Length(1),                                         // body label
            Constraint::Length(4),                                         // body editor
            Constraint::Length(1),                                         // asserts label
            Constraint::Length(section_height(0, form.asserts.len())),     // asserts table
            Constraint::Length(1),                                         // captures label
            Constraint::Length(section_height(1, form.captures.len())),    // captures table
        ])
        .split(rows[5]);

        draw_headers_section(f, sub[0], sub[1], form, s, th);
        draw_cookies_section(f, sub[2], sub[3], form, s, th);
        draw_form_section(f, sub[4], sub[5], form, s, th);
        draw_body_section(f, sub[6], sub[7], form, s, th);
        draw_asserts_section(f, sub[8], sub[9], form, s, th);
        draw_captures_section(f, sub[10], sub[11], form, s, th);
    } else {
        // A single section tab is active: give it essentially the whole
        // remaining dialog body instead of a fixed sliver, so long lists are
        // far more visible. It reads/writes the exact same `form` fields as
        // the "All" view, so nothing here is a separate copy of the data.
        let sub = Layout::vertical([
            Constraint::Length(1), // section label
            Constraint::Min(3),    // section content
        ])
        .split(rows[5]);
        match form.view_tab {
            WizardTab::All => unreachable!(),
            WizardTab::Headers => draw_headers_section(f, sub[0], sub[1], form, s, th),
            WizardTab::Cookies => draw_cookies_section(f, sub[0], sub[1], form, s, th),
            WizardTab::Form => draw_form_section(f, sub[0], sub[1], form, s, th),
            WizardTab::Body => draw_body_section(f, sub[0], sub[1], form, s, th),
            WizardTab::Asserts => draw_asserts_section(f, sub[0], sub[1], form, s, th),
            WizardTab::Captures => draw_captures_section(f, sub[0], sub[1], form, s, th),
        }
    }

    // The header-name suggestion dropdown draws last so it overlays the form.
    draw_key_suggestions(f, form, s, th);
    draw_kind_dropdown(f, form, s, th);
    draw_content_type_dropdown(f, form, s, th);
}

/// Draw the section-view tab bar (`All │ Headers │ Cookies │ Form │ Body │
/// Asserts │ Captures`), highlighting the active tab. Cycled with
/// PageUp/PageDown and reordered with Ctrl+Shift+Left/Right (rendered in
/// `order`, so a reorder is reflected immediately); purely a layout choice,
/// so switching tabs never loses or duplicates data.
fn draw_wizard_tab_bar(
    f: &mut Frame,
    area: Rect,
    active: WizardTab,
    order: &[WizardTab],
    s: &Strings,
    th: &Theme,
) {
    let mut spans = Vec::new();
    for (i, tab) in order.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ", Style::default().fg(th.dim)));
        }
        let is_active = *tab == active;
        let style = if is_active {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.dim)
        };
        spans.push(Span::styled(format!(" {} ", tab.label(s)), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw the Headers label + table into the given (label, table) rects.
fn draw_headers_section(
    f: &mut Frame,
    label: Rect,
    table: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let hdr_focused =
        matches!(form.focus, NewField::Header(..)) || form.focus == NewField::AddHeader;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_headers.to_string(),
            Style::default()
                .fg(if hdr_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );
    draw_header_table(f, table, form, s, th);
}

/// Draw the Cookies label + table into the given (label, table) rects.
fn draw_cookies_section(
    f: &mut Frame,
    label: Rect,
    table: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let cookie_focused =
        matches!(form.focus, NewField::Cookie(..)) || form.focus == NewField::AddCookie;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_cookies.to_string(),
            Style::default()
                .fg(if cookie_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );
    draw_cookie_table(f, table, form, s, th);
}

/// Draw the Form label + table into the given (label, table) rects.
fn draw_form_section(
    f: &mut Frame,
    label: Rect,
    table: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let form_focused =
        matches!(form.focus, NewField::FormField(..)) || form.focus == NewField::AddFormField;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_form.to_string(),
            Style::default()
                .fg(if form_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );
    draw_form_table(f, table, form, s, th);
}

/// Draw the Body label + editor into the given (label, editor) rects. Adds a
/// scrollbar (leftmost column, kept close to the text like the table
/// sections' scrollbars) whenever the body has more lines than fit.
fn draw_body_section(
    f: &mut Frame,
    label: Rect,
    editor: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let body_focused = form.focus == NewField::Body;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_body.to_string(),
            Style::default()
                .fg(if body_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );

    let total = form.body.lines.len();
    let capacity = editor.height as usize;
    let scrolling = capacity > 0 && total > capacity;
    let text_area = if scrolling {
        Rect {
            x: editor.x + 1,
            y: editor.y,
            width: editor.width.saturating_sub(1),
            height: editor.height,
        }
    } else {
        editor
    };

    if body_focused {
        render_editor(f, text_area, &form.body, false, th);
    } else {
        f.render_widget(
            Paragraph::new(form.body.text()).style(Style::default().fg(th.text)),
            text_area,
        );
    }

    if scrolling {
        // Mirrors `render_editor`'s own scroll calculation when focused, so
        // the bar's thumb tracks the cursor; parked at the top when
        // unfocused, matching the unfocused Paragraph's unscrolled render.
        let start = if body_focused {
            form.body.row.saturating_sub(capacity - 1)
        } else {
            0
        };
        let bar_area = Rect {
            x: editor.x,
            y: editor.y,
            width: 1,
            height: editor.height,
        };
        draw_scrollbar(f, bar_area, total, capacity, start, th);
    }
}

/// Draw the Asserts label + table into the given (label, table) rects.
fn draw_asserts_section(
    f: &mut Frame,
    label: Rect,
    table: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let assert_focused =
        matches!(form.focus, NewField::Assert(_)) || form.focus == NewField::AddAssert;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_asserts.to_string(),
            Style::default()
                .fg(if assert_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );
    draw_assert_table(f, table, form, s, th);
}

/// Draw the Captures label + table into the given (label, table) rects.
fn draw_captures_section(
    f: &mut Frame,
    label: Rect,
    table: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let cap_focused =
        matches!(form.focus, NewField::Capture(..)) || form.focus == NewField::AddCapture;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_captures.to_string(),
            Style::default()
                .fg(if cap_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        label,
    );
    draw_capture_table(f, table, form, s, th);
}

/// Draw the Text/File dropdown beneath the focused Form Kind cell, with the
/// row's current kind highlighted.
pub(crate) fn draw_kind_dropdown(f: &mut Frame, form: &NewReq, s: &Strings, th: &Theme) {
    if !form.kind_dropdown_open() {
        return;
    }
    let NewField::FormField(i, FormCol::Kind) = form.focus else {
        return;
    };
    let Some(row) = form.form_fields.get(i) else {
        return;
    };
    let Some(anchor) = form.kind_cell_rect.get() else {
        return;
    };
    let fr = f.area();
    let options = [s.form_type_text, s.form_type_file, s.form_type_base64file];
    let selected = Some(match row.kind {
        FormFieldKind::Text => 0,
        FormFieldKind::File => 1,
        FormFieldKind::Base64File => 2,
    });

    let content_w = options.iter().map(|o| o.len()).max().unwrap_or(0) as u16;
    let w = (content_w + 4).max(anchor.width).max(10).min(fr.width);
    let h = (options.len() as u16 + 2).min(fr.height);

    let mut x = anchor.x;
    if x + w > fr.right() {
        x = fr.right().saturating_sub(w);
    }
    let y = if anchor.y + 1 + h <= fr.bottom() {
        anchor.y + 1
    } else {
        anchor.y.saturating_sub(h)
    };
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, popup);
    let items: Vec<ListItem> = options
        .iter()
        .map(|opt| {
            ListItem::new(Line::styled(
                (*opt).to_string(),
                Style::default().fg(th.text),
            ))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent))
        .title(Span::styled(
            format!(" {} ", s.hdr_type),
            Style::default().fg(th.dim),
        ));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{203a} ");
    let mut st = ListState::default();
    st.select(selected);
    f.render_stateful_widget(list, popup, &mut st);
}

/// Draw the content-type override dropdown beneath a `File`-kind Form row's
/// focused Content-Type cell: "Auto" (clears the override, letting Hurl infer
/// the type from the file extension) followed by the MIME-type list from
/// [`content_type_options`], both filtered down to whatever the user has
/// typed so far (see [`NewReq::ctype_dropdown_entries`]). The row's current
/// value is pre-selected when it matches one of the options.
pub(crate) fn draw_content_type_dropdown(f: &mut Frame, form: &NewReq, s: &Strings, th: &Theme) {
    if !form.ctype_dropdown_open() {
        return;
    }
    let NewField::FormField(i, FormCol::Ctype) = form.focus else {
        return;
    };
    if form.form_fields.get(i).is_none() {
        return;
    }
    let Some(anchor) = form.ctype_cell_rect.get() else {
        return;
    };
    let fr = f.area();
    let entries = form.ctype_dropdown_entries(s);
    let labels: Vec<String> = entries
        .iter()
        .map(|e| {
            e.map(|m| m.to_string())
                .unwrap_or_else(|| s.content_type_auto.to_string())
        })
        .collect();
    let selected = form.ctype_selected_index(s);

    let content_w = labels.iter().map(|o| o.len()).max().unwrap_or(0) as u16;
    let w = (content_w + 4).max(anchor.width).max(10).min(fr.width);
    let list_h = (labels.len() as u16).min(8);
    let h = (list_h + 2).min(fr.height);

    let mut x = anchor.x;
    if x + w > fr.right() {
        x = fr.right().saturating_sub(w);
    }
    let y = if anchor.y + 1 + h <= fr.bottom() {
        anchor.y + 1
    } else {
        anchor.y.saturating_sub(h)
    };
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, popup);
    let items: Vec<ListItem> = labels
        .iter()
        .map(|opt| ListItem::new(Line::styled(opt.clone(), Style::default().fg(th.text))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent))
        .title(Span::styled(
            format!(" {} ", s.content_type_hint),
            Style::default().fg(th.dim),
        ));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{203a} ");
    let mut st = ListState::default();
    st.select(selected);
    f.render_stateful_widget(list, popup, &mut st);
}

/// Draw the filterable header-name dropdown beneath the focused Key cell.
pub(crate) fn draw_key_suggestions(f: &mut Frame, form: &NewReq, s: &Strings, th: &Theme) {
    let Some((_, sugs)) = form.key_dropdown() else {
        return;
    };
    let Some(anchor) = form.key_cell_rect.get() else {
        return;
    };
    let fr = f.area();

    let content_w = sugs.iter().map(|h| h.len()).max().unwrap_or(0) as u16;
    let w = (content_w + 4).max(anchor.width).max(16).min(fr.width);
    let list_h = (sugs.len() as u16).min(8);
    let h = (list_h + 2).min(fr.height);

    let mut x = anchor.x;
    if x + w > fr.right() {
        x = fr.right().saturating_sub(w);
    }
    // Prefer to open below the cell; flip above if there is no room.
    let y = if anchor.y + 1 + h <= fr.bottom() {
        anchor.y + 1
    } else {
        anchor.y.saturating_sub(h)
    };
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, popup);
    let items: Vec<ListItem> = sugs
        .iter()
        .map(|hname| {
            ListItem::new(Line::styled(
                (*hname).to_string(),
                Style::default().fg(th.text),
            ))
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent))
        .title(Span::styled(
            format!(" {} ", s.suggest_hint),
            Style::default().fg(th.dim),
        ));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(th.accent)
                .fg(th.bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{203a} ");
    let mut st = ListState::default();
    st.select(form.suggest_hi);
    f.render_stateful_widget(list, popup, &mut st);
}

/// Compute the first visible index of a scrollable, `total`-item list so
/// that `capacity` items are shown at once and `focused` (if any, an index
/// into the same list) stays in view. `scroll` remembers the offset between
/// frames (e.g. so a mouse/no-op redraw doesn't reset it) and is updated in
/// place. Returns the offset to use for this frame.
pub(crate) fn scroll_window(
    scroll: &std::cell::Cell<usize>,
    focused: Option<usize>,
    total: usize,
    capacity: usize,
) -> usize {
    if capacity == 0 || total <= capacity {
        scroll.set(0);
        return 0;
    }
    let max_start = total - capacity;
    let mut start = scroll.get().min(max_start);
    if let Some(idx) = focused {
        if idx < start {
            start = idx;
        } else if idx >= start + capacity {
            start = idx + 1 - capacity;
        }
    }
    scroll.set(start);
    start
}

/// Render a vertical scrollbar in `area` (expected to be a single-column
/// strip) reflecting how much of a `total`-item, `capacity`-visible list is
/// currently scrolled past `start`. No-op when everything already fits.
pub(crate) fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    capacity: usize,
    start: usize,
    th: &Theme,
) {
    if area.width == 0 || area.height == 0 || total <= capacity {
        return;
    }
    let mut state = ScrollbarState::new(total - capacity).position(start);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("\u{2502}"))
        .thumb_symbol("\u{2588}")
        .style(Style::default().fg(th.dim))
        .thumb_style(Style::default().fg(th.accent));
    f.render_stateful_widget(bar, area, &mut state);
}

/// Lay out a scrollable table's rows so that the trailing "Add ..." hint row
/// always stays visible and reachable, even when there are more data rows
/// than fit on screen. When everything fits, this is just a plain stack (an
/// optional column-header line, then each data row, then the Add row) with
/// no reserved gap. When it doesn't fit, the *data* rows alone are windowed
/// and scrolled (via `scroll_window`) while the Add row is pinned as the
/// table's last line, so scrolling through a long list never scrolls the
/// Add row out of view.
///
/// Returns `None` when `area` isn't even tall enough for the column-header
/// line (the caller should fall back to a minimal header-only rendering).
/// Otherwise returns `(table_area, header_rect, data_rects, add_rect,
/// scrolling, start)`: `table_area` is `area` narrowed by one column when
/// `scrolling` (to make room for a scrollbar strip), `data_rects[k]`
/// corresponds to data row `start + k`, and `add_rect` is always `Some`.
#[allow(clippy::type_complexity)]
fn windowed_table_rows(
    area: Rect,
    has_header: bool,
    data_len: usize,
    scroll: &std::cell::Cell<usize>,
    focused_data_idx: Option<usize>,
) -> Option<(Rect, Option<Rect>, Vec<Rect>, Rect, bool, usize)> {
    if area.height == 0 || area.width == 0 {
        return None;
    }
    let header_h = if has_header { 1usize } else { 0 };
    let avail = (area.height as usize).saturating_sub(header_h);
    if avail == 0 {
        return None;
    }

    let fits_all = data_len < avail;
    let (table_area, scrolling, start, visible) = if fits_all {
        scroll.set(0);
        (area, false, 0, data_len)
    } else {
        // Reserve the leftmost column for the scrollbar (kept close to the
        // data it belongs to) rather than the rightmost.
        let table_area = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(1),
            ..area
        };
        let data_capacity = avail - 1; // minus the pinned Add line
        let start = scroll_window(scroll, focused_data_idx, data_len, data_capacity);
        let visible = data_capacity.min(data_len - start);
        (table_area, true, start, visible)
    };

    let mut cons = Vec::new();
    if has_header {
        cons.push(Constraint::Length(1));
    }
    cons.extend(std::iter::repeat_n(Constraint::Length(1), visible + 1)); // data rows + pinned Add
    let vlines = Layout::vertical(cons).split(table_area);

    let mut idx = 0;
    let header_rect = if has_header {
        let r = vlines[idx];
        idx += 1;
        Some(r)
    } else {
        None
    };
    let data_rects = (0..visible).map(|k| vlines[idx + k]).collect::<Vec<_>>();
    let add_rect = vlines[idx + visible];
    Some((
        table_area,
        header_rect,
        data_rects,
        add_rect,
        scrolling,
        start,
    ))
}

/// Fixed width of the "Enabled" checkbox column (`[x]`).
pub(crate) const ENABLED_W: u16 = 3;

/// Column widths for the header table: `(enabled, key, value, desc)`.
/// The Enabled checkbox is a fixed narrow column. Description has the lowest
/// priority: it only appears once Key and Value fit comfortably, and is the
/// first to be dropped (width 0) as the available width shrinks.
pub(crate) fn header_widths(total: u16) -> (u16, u16, u16, u16) {
    let en = ENABLED_W;
    // Full 4-column layout needs: en + key(18) + value(24) + desc(>=6) + 3 gaps.
    if total >= en + 18 + 24 + 6 + 3 {
        let key = 18;
        let value = 24;
        let desc = total - en - key - value - 3;
        (en, key, value, desc)
    } else if total >= en + 4 + 2 {
        // Drop Description; split the remainder between Key and Value.
        let rest = total - en - 2; // two gaps: enabled|key, key|value
        let key = rest / 2;
        (en, key, rest - key, 0)
    } else {
        let rest = total.saturating_sub(en + 2).max(2);
        let key = (rest / 2).max(1);
        (en, key, rest.saturating_sub(key).max(1), 0)
    }
}

/// Split one table row into cell rectangles (a one-column gap between cells).
pub(crate) fn header_cell_rects(area: Rect, en: u16, kw: u16, vw: u16, dw: u16) -> Vec<Rect> {
    let cons: Vec<Constraint> = if dw > 0 {
        vec![
            Constraint::Length(en),
            Constraint::Length(kw),
            Constraint::Length(vw),
            Constraint::Length(dw),
        ]
    } else {
        vec![
            Constraint::Length(en),
            Constraint::Length(kw),
            Constraint::Length(vw),
        ]
    };
    Layout::horizontal(cons).spacing(1).split(area).to_vec()
}

pub(crate) fn draw_header_cell(f: &mut Frame, area: Rect, ed: &Editor, focused: bool, th: &Theme) {
    if focused {
        render_editor(f, area, ed, false, th);
    } else {
        f.render_widget(
            Paragraph::new(ed.text()).style(Style::default().fg(th.text)),
            area,
        );
    }
}

/// Render the enabled checkbox (`[x]`/`[ ]`); focus is shown by reversing it,
/// since a checkbox carries no text cursor.
pub(crate) fn draw_checkbox_cell(
    f: &mut Frame,
    area: Rect,
    enabled: bool,
    focused: bool,
    th: &Theme,
) {
    let mark = if enabled { "[x]" } else { "[ ]" };
    let mut style = Style::default().fg(if enabled { th.ok } else { th.dim });
    if focused {
        style = style.add_modifier(Modifier::REVERSED);
    }
    f.render_widget(Paragraph::new(Span::styled(mark, style)), area);
}

/// Shared drawing logic for a `HeaderRow`-shaped table (used by both the
/// Headers and Cookies sections, which share an identical column layout).
/// Windows and scrolls the row list when it doesn't fit in `area`, drawing a
/// scrollbar in the reclaimed rightmost column when it does.
#[allow(clippy::too_many_arguments)]
fn draw_headerlike_table(
    f: &mut Frame,
    area: Rect,
    rows: &[HeaderRow],
    desc_visible: &std::cell::Cell<bool>,
    scroll: &std::cell::Cell<usize>,
    key_cell_rect: Option<&std::cell::Cell<Option<Rect>>>,
    focused_idx: Option<usize>,
    is_col_focused: impl Fn(usize, HdrCol) -> bool,
    add_focused: bool,
    add_label: &str,
    s: &Strings,
    th: &Theme,
) {
    if let Some(kcr) = key_cell_rect {
        kcr.set(None);
    }
    let Some((table_area, header_rect, data_rects, add_rect, scrolling, start)) =
        windowed_table_rows(area, true, rows.len(), scroll, focused_idx)
    else {
        return;
    };

    let (en, kw, vw, dw) = header_widths(table_area.width);
    desc_visible.set(dw > 0);

    let lbl = |t: &str| {
        Paragraph::new(Span::styled(
            t.to_string(),
            Style::default().fg(th.dim).add_modifier(Modifier::BOLD),
        ))
    };

    if let Some(hrect) = header_rect {
        let hcells = header_cell_rects(hrect, en, kw, vw, dw);
        f.render_widget(lbl("\u{2713}"), hcells[0]); // ✓ header for the enabled column
        f.render_widget(lbl(s.hdr_key), hcells[1]);
        f.render_widget(lbl(s.hdr_value), hcells[2]);
        if dw > 0 {
            f.render_widget(lbl(s.hdr_description), hcells[3]);
        }
    }

    for (slot, row_area) in data_rects.iter().enumerate() {
        let i = start + slot;
        let row = &rows[i];
        let cells = header_cell_rects(*row_area, en, kw, vw, dw);
        draw_checkbox_cell(
            f,
            cells[0],
            row.enabled,
            is_col_focused(i, HdrCol::Enabled),
            th,
        );
        draw_header_cell(f, cells[1], &row.key, is_col_focused(i, HdrCol::Key), th);
        draw_header_cell(
            f,
            cells[2],
            &row.value,
            is_col_focused(i, HdrCol::Value),
            th,
        );
        if dw > 0 {
            draw_header_cell(f, cells[3], &row.desc, is_col_focused(i, HdrCol::Desc), th);
        }
        // Remember the Key cell's position so the suggestion dropdown can
        // anchor beneath it (drawn later, on top of the form).
        if is_col_focused(i, HdrCol::Key)
            && let Some(kcr) = key_cell_rect
        {
            kcr.set(Some(cells[1]));
        }
    }

    f.render_widget(
        Paragraph::new(Span::styled(
            add_label.to_string(),
            Style::default()
                .fg(if add_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        add_rect,
    );

    if scrolling {
        let header_h = 1u16;
        let bar_area = Rect {
            x: area.x, // leftmost column: keep the scrollbar close to the data
            y: table_area.y + header_h,
            width: 1,
            height: table_area.height.saturating_sub(header_h),
        };
        draw_scrollbar(f, bar_area, rows.len(), data_rects.len().max(1), start, th);
    }
}

pub(crate) fn draw_header_table(f: &mut Frame, area: Rect, form: &NewReq, s: &Strings, th: &Theme) {
    let focused_idx = match form.focus {
        NewField::Header(i, _) => Some(i),
        _ => None,
    };
    draw_headerlike_table(
        f,
        area,
        &form.headers,
        &form.desc_visible,
        &form.header_scroll,
        Some(&form.key_cell_rect),
        focused_idx,
        |i, col| form.focus == NewField::Header(i, col),
        form.focus == NewField::AddHeader,
        s.add_header,
        s,
        th,
    );
}

/// Draw the `[Cookies]` table: same column layout as Headers, but a separate
/// list/scroll state and no header-name suggestion dropdown.
pub(crate) fn draw_cookie_table(f: &mut Frame, area: Rect, form: &NewReq, s: &Strings, th: &Theme) {
    let focused_idx = match form.focus {
        NewField::Cookie(i, _) => Some(i),
        _ => None,
    };
    draw_headerlike_table(
        f,
        area,
        &form.cookies,
        &form.cookie_desc_visible,
        &form.cookie_scroll,
        None,
        focused_idx,
        |i, col| form.focus == NewField::Cookie(i, col),
        form.focus == NewField::AddCookie,
        s.add_cookie,
        s,
        th,
    );
}

/// Column widths for the Form/Multipart table: `(enabled, key, value, kind,
/// ctype, prefix, desc)`. Description is the first to be dropped as width
/// shrinks, then Base64 Prefix, then Content-Type, then the Kind dropdown
/// cell (fixed `"File \u{25be}"`-width) itself.
pub(crate) fn form_widths(total: u16) -> (u16, u16, u16, u16, u16, u16, u16) {
    let en = ENABLED_W;
    let kind_w = 12u16;
    let ctype_w = 18u16;
    let prefix_w = 16u16;
    let key = 14u16;
    let value = 20u16;
    // Widest layout: every column including Prefix and Desc.
    if total >= en + kind_w + ctype_w + prefix_w + key + value + 6 + 6 {
        let desc = total - en - kind_w - ctype_w - prefix_w - key - value - 6;
        (en, key, value, kind_w, ctype_w, prefix_w, desc)
    } else if total >= en + kind_w + ctype_w + prefix_w + key + value + 5 {
        // Room for Prefix + Ctype but not Desc.
        (en, key, value, kind_w, ctype_w, prefix_w, 0)
    } else if total >= en + kind_w + ctype_w + key + value + 4 {
        // Room for Ctype but neither Prefix nor Desc.
        (en, key, value, kind_w, ctype_w, 0, 0)
    } else if total >= en + kind_w + 4 + 3 {
        let rest = total - en - kind_w - 3;
        let key = (rest / 2).max(1);
        (en, key, rest.saturating_sub(key).max(1), kind_w, 0, 0, 0)
    } else {
        // Extremely narrow: drop Kind too, minimal Key/Value only.
        let rest = total.saturating_sub(en + 2).max(2);
        let key = (rest / 2).max(1);
        (en, key, rest.saturating_sub(key).max(1), 0, 0, 0, 0)
    }
}

/// Split one Form table row into cell rectangles; Kind/Ctype/Prefix/Desc
/// columns are omitted entirely when their width is 0 (mirrors
/// `header_cell_rects`). Column order: Enabled, Key, Kind (if shown), Value,
/// Ctype (if shown), Prefix (if shown), Desc (if shown) — Kind sits before
/// Value so filling a row top-to-bottom naturally chooses the type before
/// typing the value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn form_cell_rects(
    area: Rect,
    en: u16,
    kw: u16,
    vw: u16,
    kindw: u16,
    ctypew: u16,
    prefixw: u16,
    dw: u16,
) -> Vec<Rect> {
    let mut cons = vec![Constraint::Length(en), Constraint::Length(kw)];
    if kindw > 0 {
        cons.push(Constraint::Length(kindw));
    }
    cons.push(Constraint::Length(vw));
    if ctypew > 0 {
        cons.push(Constraint::Length(ctypew));
    }
    if prefixw > 0 {
        cons.push(Constraint::Length(prefixw));
    }
    if dw > 0 {
        cons.push(Constraint::Length(dw));
    }
    Layout::horizontal(cons).spacing(1).split(area).to_vec()
}

/// Colour a Form row's Value cell by file-existence when it's a `File` field:
/// `th.ok` when the path resolves (relative to `file_root`, or the current
/// directory when unset) to an existing file, `th.err` otherwise. Text
/// fields and empty paths use the normal text colour.
fn form_value_color(row: &FormRow, file_root: Option<&PathBuf>, th: &Theme) -> Color {
    if !row.kind.is_multipart() {
        return th.text;
    }
    let text = row.value.text();
    if text.is_empty() {
        return th.text;
    }
    let path = PathBuf::from(text);
    let resolved = if path.is_absolute() {
        path
    } else {
        file_root
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(path)
    };
    if resolved.is_file() { th.ok } else { th.err }
}

/// Draw the `[Form]`/`[Multipart]` table: Enabled/Key/Type/Value/Content-Type/
/// Description columns (Type comes before Value so filling a row naturally
/// picks Text/File before typing/choosing the value). The Type column opens
/// a small Text/File dropdown (like the Key suggestion dropdown for
/// Headers); the Content-Type column opens the MIME-type dropdown for
/// `File`-kind rows; File-kind Value cells are colour-highlighted by
/// whether the path resolves to a reachable file.
pub(crate) fn draw_form_table(f: &mut Frame, area: Rect, form: &NewReq, s: &Strings, th: &Theme) {
    form.kind_cell_rect.set(None);
    form.ctype_cell_rect.set(None);
    let focused_idx = match form.focus {
        NewField::FormField(i, _) => Some(i),
        _ => None,
    };
    let Some((table_area, header_rect, data_rects, add_rect, scrolling, start)) =
        windowed_table_rows(
            area,
            true,
            form.form_fields.len(),
            &form.form_scroll,
            focused_idx,
        )
    else {
        return;
    };

    let (en, kw, vw, kindw, ctypew, prefixw, dw) = form_widths(table_area.width);
    form.form_desc_visible.set(dw > 0);
    form.form_ctype_visible.set(ctypew > 0);
    form.form_prefix_visible.set(prefixw > 0);

    let lbl = |t: &str| {
        Paragraph::new(Span::styled(
            t.to_string(),
            Style::default().fg(th.dim).add_modifier(Modifier::BOLD),
        ))
    };
    // Column order: [0]=Enabled, [1]=Key, [kind_idx]=Kind (if shown),
    // [value_idx]=Value, [ctype_idx]=Ctype (if shown), [prefix_idx]=Prefix
    // (if shown), [desc_idx]=Desc (if shown).
    let value_idx = if kindw > 0 { 3 } else { 2 };
    let mut next = value_idx + 1;
    let ctype_idx = next;
    if ctypew > 0 {
        next += 1;
    }
    let prefix_idx = next;
    if prefixw > 0 {
        next += 1;
    }
    let desc_idx = next;

    if let Some(hrect) = header_rect {
        let hcells = form_cell_rects(hrect, en, kw, vw, kindw, ctypew, prefixw, dw);
        f.render_widget(lbl("\u{2713}"), hcells[0]);
        f.render_widget(lbl(s.hdr_key), hcells[1]);
        if kindw > 0 {
            f.render_widget(lbl(s.hdr_type), hcells[2]);
        }
        f.render_widget(lbl(s.hdr_value), hcells[value_idx]);
        if ctypew > 0 {
            f.render_widget(lbl(s.content_type_hint), hcells[ctype_idx]);
        }
        if prefixw > 0 {
            f.render_widget(lbl(s.hdr_base64_prefix), hcells[prefix_idx]);
        }
        if dw > 0 {
            f.render_widget(lbl(s.hdr_description), hcells[desc_idx]);
        }
    }

    for (slot, row_area) in data_rects.iter().enumerate() {
        let i = start + slot;
        let row = &form.form_fields[i];
        let cells = form_cell_rects(*row_area, en, kw, vw, kindw, ctypew, prefixw, dw);
        draw_checkbox_cell(
            f,
            cells[0],
            row.enabled,
            form.focus == NewField::FormField(i, FormCol::Enabled),
            th,
        );
        draw_header_cell(
            f,
            cells[1],
            &row.key,
            form.focus == NewField::FormField(i, FormCol::Key),
            th,
        );

        if kindw > 0 {
            let kind_focused = form.focus == NewField::FormField(i, FormCol::Kind);
            let label = match row.kind {
                FormFieldKind::Text => s.form_type_text,
                FormFieldKind::File => s.form_type_file,
                FormFieldKind::Base64File => s.form_type_base64file,
            };
            // A dropdown indicator (▾), not cycle arrows: Left/Right now hop
            // to the neighbouring column instead of changing the value.
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("{} \u{25be}", label),
                    Style::default()
                        .fg(if kind_focused { th.accent } else { th.text })
                        .add_modifier(Modifier::BOLD),
                )),
                cells[2],
            );
            if kind_focused {
                form.kind_cell_rect.set(Some(cells[2]));
            }
        }

        let value_focused = form.focus == NewField::FormField(i, FormCol::Value);
        // Show a folder icon at the left edge of a File/Base64File-kind row's
        // Value cell — both pick a file — anchoring it to the Value field, as
        // a hint that pressing Enter (or Ctrl+F) opens a file picker, not
        // otherwise obvious since the cell looks like plain text entry.
        let value_is_file = row.kind.is_multipart();
        let (file_icon_rect, text_rect) = if value_is_file && cells[value_idx].width > 2 {
            let split = Layout::horizontal([Constraint::Length(2), Constraint::Min(1)])
                .split(cells[value_idx]);
            (Some(split[0]), split[1])
        } else {
            (None, cells[value_idx])
        };
        if value_focused {
            render_editor(f, text_rect, &row.value, false, th);
        } else {
            let color = form_value_color(row, form.file_root.as_ref(), th);
            f.render_widget(
                Paragraph::new(row.value.text()).style(Style::default().fg(color)),
                text_rect,
            );
        }
        if let Some(icon_rect) = file_icon_rect {
            let icon_color = if value_focused { th.accent } else { th.dim };
            f.render_widget(
                Paragraph::new(Span::styled(FOLDER_ICON, Style::default().fg(icon_color))),
                icon_rect,
            );
        }

        if ctypew > 0 {
            let ctype_focused = form.focus == NewField::FormField(i, FormCol::Ctype);
            // An empty Content-Type cell means Hurl will auto-detect it at
            // send time; show a dimmed "Auto" placeholder instead of a blank
            // cell so it's clear a value is actually in effect.
            if !ctype_focused
                && row.kind == FormFieldKind::File
                && row.ctype.text().trim().is_empty()
            {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        s.content_type_auto_placeholder,
                        Style::default().fg(th.dim),
                    )),
                    cells[ctype_idx],
                );
            } else if row.kind == FormFieldKind::Base64File {
                // Content-Type doesn't apply to a Base64File (it's sent as
                // plain text): leave the cell blank rather than editable.
            } else {
                draw_header_cell(f, cells[ctype_idx], &row.ctype, ctype_focused, th);
            }
            if ctype_focused && row.kind == FormFieldKind::File {
                form.ctype_cell_rect.set(Some(cells[ctype_idx]));
            }
        }

        if prefixw > 0 {
            let prefix_focused = form.focus == NewField::FormField(i, FormCol::Prefix);
            // The Base64 Prefix only affects Base64File rows; on other kinds
            // it's an inert cell (still editable, but ignored on save). Show a
            // dimmed placeholder for an empty, unfocused non-Base64File cell
            // so the column reads as inactive there, otherwise the editor.
            if row.kind != FormFieldKind::Base64File
                && !prefix_focused
                && row.base64_prefix.text().is_empty()
            {
                f.render_widget(
                    Paragraph::new(Span::styled("\u{2014}", Style::default().fg(th.dim))),
                    cells[prefix_idx],
                );
            } else {
                draw_header_cell(f, cells[prefix_idx], &row.base64_prefix, prefix_focused, th);
            }
        }

        if dw > 0 {
            let desc_focused = form.focus == NewField::FormField(i, FormCol::Desc);
            draw_header_cell(f, cells[desc_idx], &row.desc, desc_focused, th);
        }
    }

    let add_focused = form.focus == NewField::AddFormField;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.add_form_field.to_string(),
            Style::default()
                .fg(if add_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        add_rect,
    );

    if scrolling {
        let header_h = 1u16;
        let bar_area = Rect {
            x: area.x, // leftmost column: keep the scrollbar close to the data
            y: table_area.y + header_h,
            width: 1,
            height: table_area.height.saturating_sub(header_h),
        };
        draw_scrollbar(
            f,
            bar_area,
            form.form_fields.len(),
            data_rects.len().max(1),
            start,
            th,
        );
    }
}

/// Draw the `[Asserts]` table: one raw Hurl assert expression per row, plus
/// an "Add assert" hint line. No column header, so the whole area's height
/// is available as scroll capacity.
pub(crate) fn draw_assert_table(f: &mut Frame, area: Rect, form: &NewReq, s: &Strings, th: &Theme) {
    let focused_idx = match form.focus {
        NewField::Assert(i) => Some(i),
        _ => None,
    };
    let Some((table_area, _, data_rects, add_rect, scrolling, start)) = windowed_table_rows(
        area,
        false,
        form.asserts.len(),
        &form.assert_scroll,
        focused_idx,
    ) else {
        return;
    };

    for (slot, row_area) in data_rects.iter().enumerate() {
        let i = start + slot;
        draw_header_cell(
            f,
            *row_area,
            &form.asserts[i].expr,
            form.focus == NewField::Assert(i),
            th,
        );
    }

    let add_focused = form.focus == NewField::AddAssert;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.add_assert.to_string(),
            Style::default()
                .fg(if add_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        add_rect,
    );

    if scrolling {
        // Leftmost column: keep the scrollbar close to the data.
        let bar_area = Rect {
            x: area.x,
            y: table_area.y,
            width: 1,
            height: table_area.height,
        };
        draw_scrollbar(
            f,
            bar_area,
            form.asserts.len(),
            data_rects.len().max(1),
            start,
            th,
        );
    }
}

/// Draw the `[Captures]` table: `Name | Expression` columns, plus an "Add
/// capture" hint line.
pub(crate) fn draw_capture_table(
    f: &mut Frame,
    area: Rect,
    form: &NewReq,
    s: &Strings,
    th: &Theme,
) {
    let focused_idx = match form.focus {
        NewField::Capture(i, _) => Some(i),
        _ => None,
    };
    let Some((table_area, header_rect, data_rects, add_rect, scrolling, start)) =
        windowed_table_rows(
            area,
            true,
            form.captures.len(),
            &form.capture_scroll,
            focused_idx,
        )
    else {
        return;
    };

    let name_w = 18u16.min(table_area.width.saturating_sub(4)).max(4);
    let cell_rects = |row_area: Rect| {
        Layout::horizontal([Constraint::Length(name_w), Constraint::Min(1)])
            .spacing(1)
            .split(row_area)
    };
    let lbl = |t: &str| {
        Paragraph::new(Span::styled(
            t.to_string(),
            Style::default().fg(th.dim).add_modifier(Modifier::BOLD),
        ))
    };

    if let Some(hrect) = header_rect {
        let hcells = cell_rects(hrect);
        f.render_widget(lbl(s.cap_name), hcells[0]);
        f.render_widget(lbl(s.cap_expr), hcells[1]);
    }

    for (slot, row_area) in data_rects.iter().enumerate() {
        let i = start + slot;
        let row = &form.captures[i];
        let cells = cell_rects(*row_area);
        draw_header_cell(
            f,
            cells[0],
            &row.name,
            form.focus == NewField::Capture(i, CapCol::Name),
            th,
        );
        draw_header_cell(
            f,
            cells[1],
            &row.expr,
            form.focus == NewField::Capture(i, CapCol::Expr),
            th,
        );
    }

    let add_focused = form.focus == NewField::AddCapture;
    f.render_widget(
        Paragraph::new(Span::styled(
            s.add_capture.to_string(),
            Style::default()
                .fg(if add_focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        add_rect,
    );

    if scrolling {
        let header_h = 1u16;
        let bar_area = Rect {
            x: area.x, // leftmost column: keep the scrollbar close to the data
            y: table_area.y + header_h,
            width: 1,
            height: table_area.height.saturating_sub(header_h),
        };
        draw_scrollbar(
            f,
            bar_area,
            form.captures.len(),
            data_rects.len().max(1),
            start,
            th,
        );
    }
}

/// URL row that shows the Base URL as a light-grey ghost while the field is
/// empty; Right arrow (handled in the key loop) commits it.
pub(crate) fn draw_url_row(f: &mut Frame, area: Rect, form: &NewReq, s: &Strings, th: &Theme) {
    let focused = form.focus == NewField::Url;
    let cols = Layout::horizontal([Constraint::Length(10), Constraint::Min(1)]).split(area);
    f.render_widget(
        Paragraph::new(Span::styled(
            s.field_url.to_string(),
            Style::default()
                .fg(if focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        cols[0],
    );
    let text = form.url.text();
    if text.is_empty() && !form.base_url.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                form.base_url.clone(),
                Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
            )),
            cols[1],
        );
        if focused {
            f.set_cursor_position(Position::new(cols[1].x, cols[1].y));
        }
    } else if focused {
        render_editor(f, cols[1], &form.url, false, th);
    } else {
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(th.text)),
            cols[1],
        );
    }
}

/// A single-line labelled field row for the New Request form; shows the editor
/// cursor when focused.
pub(crate) fn new_req_text_row(
    f: &mut Frame,
    area: Rect,
    label: &str,
    ed: &Editor,
    focused: bool,
    th: &Theme,
) {
    let cols = Layout::horizontal([Constraint::Length(10), Constraint::Min(1)]).split(area);
    f.render_widget(
        Paragraph::new(Span::styled(
            label.to_string(),
            Style::default()
                .fg(if focused { th.accent } else { th.dim })
                .add_modifier(Modifier::BOLD),
        )),
        cols[0],
    );
    if focused {
        render_editor(f, cols[1], ed, false, th);
    } else {
        f.render_widget(
            Paragraph::new(ed.text()).style(Style::default().fg(th.text)),
            cols[1],
        );
    }
}
