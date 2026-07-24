//! The TUI-side wrapper around a PaperTrail [`Report`] plus the "Reports view"
//! — a full-screen tab kind that lives in the same tab strip as the collection
//! tabs but shows only report content (no environments / response / raw-view
//! panels, so it fits small monitors, per the design).
//!
//! The core [`Report`] stays front-end agnostic; everything ratatui-specific
//! (cached diagnostics, the inline source editor, drawing) lives here so a
//! future GUI can reuse the core unchanged. A report tab shows the flow source
//! plus its live validation. Editing is *focus-based inline* (like the request
//! wizard's text cells): pressing `e`/Enter gives the source panel edit focus
//! so keystrokes type directly into it, and Esc returns to navigation mode
//! where single letters act as view shortcuts again. Edits apply live (the
//! validation panel refreshes as you type); Esc just leaves edit focus.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tui_panel_select::MultiSelectPanel;

use super::app::{Overlay, Pane, TuiApp};
use super::draw::panel;
use super::editor::{Editor, apply_edit_key_full, render_editor_highlighted};
use super::new_request::draw_scrollbar;
use super::theme::Theme;
use crate::i18n::Strings;
use crate::report::Report;
use crate::report::validate::{Context, Diagnostic, Severity, validate};

/// A report tab: the owned [`Report`] plus TUI-only view state (cached
/// diagnostics and, when the source fails to parse, the parser message). The
/// diagnostics are recomputed by [`TuiApp::revalidate_report`] whenever the
/// source, the bound collection, or the loaded environments change.
pub(crate) struct ReportTab {
    pub(crate) report: Report,
    /// Validation diagnostics (errors + warnings) from the last revalidation.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Parser error message, if the source doesn't currently parse (shown in
    /// place of `diagnostics`, which can't be computed without a parse tree).
    pub(crate) parse_error: Option<String>,
    /// 1-based source line the parser rejected, if any — the syntax highlighter
    /// underlines it so a malformed script is obvious at a glance.
    pub(crate) parse_error_line: Option<usize>,
    /// When `Some`, the source panel has *edit focus*: keystrokes type into
    /// this live buffer (mirrored into `report.text` on every edit so the
    /// validation panel and tab name stay current) instead of acting as view
    /// shortcuts. `None` = navigation mode.
    pub(crate) editor: Option<Editor>,
    /// Selection/scroll panel backing the read-only source view (so it renders
    /// with the same wrapping, scrollbar and mouse-selection feel as the
    /// collection view's panels).
    pub(crate) source_panel: MultiSelectPanel,
    /// Selection/scroll panel backing the validation output.
    pub(crate) validation_panel: MultiSelectPanel,
}

impl ReportTab {
    pub(crate) fn new(report: Report) -> Self {
        Self {
            report,
            diagnostics: Vec::new(),
            parse_error: None,
            parse_error_line: None,
            editor: None,
            source_panel: MultiSelectPanel::new(),
            validation_panel: MultiSelectPanel::new(),
        }
    }
}

impl TuiApp {
    /// Total number of tabs across both kinds (collections first, then reports).
    pub(crate) fn tab_count(&self) -> usize {
        self.collections.len() + self.reports.len()
    }

    /// Whether the active tab is a report tab (its unified index falls past the
    /// collection tabs).
    pub(crate) fn active_is_report(&self) -> bool {
        self.active_tab >= self.collections.len() && !self.reports.is_empty()
    }

    /// The `self.reports` index of the active tab, if it is a report tab.
    pub(crate) fn active_report_index(&self) -> Option<usize> {
        if self.active_is_report() {
            Some(self.active_tab - self.collections.len())
        } else {
            None
        }
    }

    pub(crate) fn active_report(&self) -> Option<&ReportTab> {
        self.active_report_index().and_then(|i| self.reports.get(i))
    }

    /// Create a new scratch report tab, make it active, validate it and persist.
    pub(crate) fn new_report_tab(&mut self) {
        let s = Strings::for_language(&self.language);
        let report = Report::scratch(s.report_default_name);
        self.reports.push(ReportTab::new(report));
        self.active_tab = self.collections.len() + self.reports.len() - 1;
        self.focus = Pane::Tabs;
        let idx = self.reports.len() - 1;
        self.revalidate_report(idx);
        self.save_state();
    }

    /// Find the loaded collection tab a report is bound to, by resolving the
    /// report's `# collection:` reference (relative to the report's own path
    /// when possible, else as an absolute path) against each open collection's
    /// path. Falls back to matching a collection by *name* so a report bound
    /// before the collection was ever saved to disk (common in tests / scratch
    /// work) still resolves. Git refs (`git:…`) aren't auto-resolved here.
    pub(crate) fn resolve_bound_collection(&self, report: &Report) -> Option<usize> {
        let cref = report.collection_ref()?;
        if cref.starts_with("git:") {
            return None;
        }
        let target = resolve_ref_path(report.path.as_deref(), &cref);
        self.collections.iter().position(|c| {
            c.path.as_ref().is_some_and(|p| paths_equal(p, &target)) || c.name == cref
        })
    }

    /// Recompute the diagnostics (and parse-error state) for report tab `idx`
    /// against the currently-loaded collections and environments.
    pub(crate) fn revalidate_report(&mut self, idx: usize) {
        // Compute the parse-error / diagnostics up front so the immutable reads
        // of `self.collections` / `self.global_envs` don't overlap the mutable
        // borrow of `self.reports[idx]` that stores the result.
        let (parse_error, parse_error_line, diagnostics) = {
            let Some(rt) = self.reports.get(idx) else {
                return;
            };
            match rt.report.flow() {
                Err(e) => (Some(e.to_string()), Some(e.line), Vec::new()),
                Ok(flow) => {
                    let titles: Option<Vec<String>> =
                        self.resolve_bound_collection(&rt.report).map(|ci| {
                            self.collections[ci]
                                .entries
                                .iter()
                                .map(|e| e.title.clone())
                                .collect()
                        });
                    let env_names: Vec<String> =
                        self.global_envs.iter().map(|e| e.name.clone()).collect();
                    let ctx = Context {
                        request_titles: titles.as_deref(),
                        env_names: Some(&env_names),
                    };
                    (None, None, validate(&flow, &ctx))
                }
            }
        };
        if let Some(rt) = self.reports.get_mut(idx) {
            rt.parse_error = parse_error;
            rt.parse_error_line = parse_error_line;
            rt.diagnostics = diagnostics;
        }
    }

    /// Give the active report tab's source panel edit focus, seeding an
    /// [`Editor`] from the current source text. While focused, keystrokes type
    /// directly into the panel (see [`TuiApp::on_key_report_editing`]).
    pub(crate) fn enter_report_edit(&mut self) {
        if let Some(idx) = self.active_report_index() {
            let text = self.reports[idx].report.text.clone();
            self.reports[idx].editor = Some(Editor::new(&text, true));
        }
    }

    /// Close the active report tab, remembering it for reopen (`u`) and moving
    /// focus to the previous tab. Unlike collection tabs, a report tab at any
    /// position is closable (only the built-in Request collection tab is fixed).
    pub(crate) fn close_active_report_tab(&mut self) {
        let Some(ridx) = self.active_report_index() else {
            return;
        };
        let rt = self.reports.remove(ridx);
        self.closed_tabs
            .push(super::app::ClosedTab::Report(ridx, Box::new(rt)));
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
        // Prefer staying on the neighbouring report; otherwise fall back to the
        // last collection tab. `active_tab` counts collections then reports.
        let base = self.collections.len();
        self.active_tab = if self.reports.is_empty() {
            base - 1
        } else {
            base + ridx.min(self.reports.len() - 1)
        };
        self.focus = Pane::Tabs;
        self.status = Some(crate::i18n::Status::TabClosed);
        self.save_state();
    }

    /// Key handling for the main view while a report tab is active. Kept
    /// separate from `on_key_normal` (which is full of `collections[active_tab]`
    /// accesses that would panic on a report's unified index) — the normal
    /// handler dispatches here at its very top. Only tab-navigation, global
    /// menu, and report-specific keys are honoured.
    pub(crate) fn on_key_report(&mut self, key: KeyEvent) {
        // When the source panel has edit focus, keystrokes type into it rather
        // than acting as view shortcuts (Esc leaves edit focus).
        if let Some(idx) = self.active_report_index()
            && self.reports[idx].editor.is_some()
        {
            self.on_key_report_editing(key, idx);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            // Tab navigation (mirrors the collection-view bindings).
            KeyCode::Char('[') | KeyCode::PageUp => self.cycle_tab(false),
            KeyCode::Char(']') | KeyCode::PageDown => self.cycle_tab(true),
            KeyCode::Left if ctrl && shift => self.move_active_tab(false),
            KeyCode::Right if ctrl && shift => self.move_active_tab(true),
            KeyCode::Left if ctrl => self.cycle_tab(false),
            KeyCode::Right if ctrl => self.cycle_tab(true),
            KeyCode::Char('w') if ctrl => self.close_active_tab(),
            KeyCode::Char('u') => self.reopen_closed_tab(),
            // Global menus, unchanged from the collection view.
            KeyCode::Char('f') => self.overlay = Some(Overlay::FileMenu(0)),
            KeyCode::Char('s') => self.overlay = Some(Overlay::Options(0)),
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help(0));
                self.help_scroll = 0;
            }
            // Open another new report.
            KeyCode::Char('R') => self.new_report_tab(),
            // Report-specific: give the source panel edit focus.
            KeyCode::Char('e') | KeyCode::Enter => self.enter_report_edit(),
            // Scroll the read-only source panel (edit focus uses these to move
            // the cursor instead). Overshoot is clamped when it next draws.
            KeyCode::Up => self.scroll_report_source(-1),
            KeyCode::Down => self.scroll_report_source(1),
            KeyCode::Home => self.scroll_report_source(i32::MIN),
            KeyCode::End => self.scroll_report_source(i32::MAX),
            _ => {}
        }
    }

    /// Nudge the active report's read-only source panel scroll by `delta` rows
    /// (`i32::MIN`/`MAX` jump to the top/bottom). The draw pass clamps to the
    /// real content height, so this doesn't need the viewport size.
    fn scroll_report_source(&mut self, delta: i32) {
        if let Some(idx) = self.active_report_index() {
            let panel = &mut self.reports[idx].source_panel;
            let next = (panel.scroll() as i32).saturating_add(delta).max(0);
            panel.set_scroll(next.min(u16::MAX as i32) as u16);
        }
    }

    /// Key handling while the active report's source panel has edit focus.
    /// Esc leaves edit focus (keeping the typed text — edits are applied live);
    /// most keys are delegated to the shared multi-line handler, but two are
    /// intercepted first: Ctrl+Left/Right move a word at a time (rather than to
    /// the line ends), and Right accepts a pending `REQUEST`-name completion.
    /// After each edit the buffer is mirrored into `report.text` and the tab is
    /// revalidated so the validation panel stays live.
    fn on_key_report_editing(&mut self, key: KeyEvent, idx: usize) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A pending ghost completion (only for a plain Right-arrow accept). This
        // is computed *before* the `&mut editor` borrow below, since it needs an
        // immutable borrow of `self` (the bound collection's request names).
        let completion = if key.code == KeyCode::Right && !ctrl && !shift {
            self.report_request_completion(idx)
        } else {
            None
        };

        let mut leave = false;
        let mut copy: Option<String> = None;
        // Apply the keystroke to the editor, then read back the (possibly)
        // updated text; the borrow of `self.reports[idx].editor` ends with this
        // block so the follow-up `revalidate_report`/`save_state` calls (which
        // borrow all of `self`) are free to run.
        let new_text: Option<String> = {
            let Some(rt) = self.reports.get_mut(idx) else {
                return;
            };
            let Some(editor) = rt.editor.as_mut() else {
                return;
            };
            match key.code {
                // Esc leaves edit focus (the live-applied text is kept).
                KeyCode::Esc => {
                    leave = true;
                    Some(editor.text())
                }
                // Ctrl+Left/Right move one word (not to the line ends), keeping
                // Shift's selection-extend behaviour.
                KeyCode::Left if ctrl => {
                    editor.set_selecting(shift);
                    word_left(editor);
                    None
                }
                KeyCode::Right if ctrl => {
                    editor.set_selecting(shift);
                    word_right(editor);
                    None
                }
                // Right arrow at end of a `REQUEST` line fills in the completion
                // (auto-quoting the name when it contains spaces).
                KeyCode::Right if completion.is_some() => {
                    accept_request_completion(editor, completion.as_ref().unwrap());
                    Some(editor.text())
                }
                // Everything else goes to the shared multi-line key handler,
                // which reports whether the text changed and any selection the
                // host should copy.
                _ => {
                    let resp = apply_edit_key_full(editor, key);
                    copy = resp.copy;
                    if resp.changed {
                        Some(editor.text())
                    } else {
                        None
                    }
                }
            }
        };

        if let Some(text) = copy {
            super::clipboard::copy_to_clipboard(&text);
            self.status = Some(crate::i18n::Status::Copied);
        }
        if let Some(text) = new_text {
            if let Some(rt) = self.reports.get_mut(idx) {
                if leave {
                    rt.editor = None;
                }
                rt.report.set_text(text);
            }
            self.revalidate_report(idx);
            if leave {
                self.save_state();
            }
        }
    }

    /// The ghost suffix to offer while typing a `REQUEST <name>` (or
    /// `REPORT REQUEST <name>`) line in the source editor: the remainder of the
    /// first request title in the bound collection that the partially-typed
    /// The request-name completion to offer while typing a `REQUEST <name>` (or
    /// `REPORT REQUEST <name>`) line in the source editor: the first request
    /// title in the bound collection that the partially-typed name is a prefix
    /// of. `None` unless the cursor is at the end of such a line and a match
    /// exists. The report view can't show the collection's request list (it
    /// takes the whole body), so this keeps request names discoverable and
    /// correct.
    ///
    /// PaperTrail requires a name that contains spaces to be quoted
    /// (`REQUEST "Two Words"`), so completion is quote-aware and always yields a
    /// parseable line:
    /// - a bare fragment matching a space-free title completes bare;
    /// - a bare fragment matching a title *with* spaces auto-quotes it (the
    ///   opening quote is inserted before the fragment on accept, so typing
    ///   `Up` completes to `"Upload document"`);
    /// - inside an opened quote, any title completes and the closing quote is
    ///   appended.
    pub(crate) fn report_request_completion(&self, idx: usize) -> Option<RequestCompletion> {
        let rt = self.reports.get(idx)?;
        let editor = rt.editor.as_ref()?;
        let line = editor.lines.get(editor.row)?;
        // Only complete when the cursor sits at the very end of the line.
        if editor.col != line.chars().count() {
            return None;
        }
        let ci = self.resolve_bound_collection(&rt.report)?;
        let mut titles = self.collections[ci]
            .entries
            .iter()
            .map(|e| e.title.as_str());
        match request_name_partial(line)? {
            // Bare token: match any title (an exact match has nothing to add, so
            // require strictly longer), auto-quoting one that contains spaces.
            NamePartial::Bare(p) => {
                let t = titles.find(|t| t.len() > p.len() && t.starts_with(&p))?;
                let suffix = t[p.len()..].to_string();
                if t.chars().any(char::is_whitespace) {
                    // Show the plain suffix (so the ghost stays visually
                    // balanced); on accept, wrap the whole token in quotes.
                    Some(RequestCompletion {
                        ghost: suffix.clone(),
                        insert: format!("{suffix}\""),
                        wrap_quote: true,
                    })
                } else {
                    Some(RequestCompletion {
                        ghost: suffix.clone(),
                        insert: suffix,
                        wrap_quote: false,
                    })
                }
            }
            // Inside an opened quote: any title is fair game (spaces are fine),
            // and the completion appends the closing quote. An exact match still
            // completes — to just the closing quote.
            NamePartial::Quoted(p) => {
                let t = titles.find(|t| t.len() >= p.len() && t.starts_with(&p))?;
                let ghost = format!("{}\"", &t[p.len()..]);
                Some(RequestCompletion {
                    insert: ghost.clone(),
                    ghost,
                    wrap_quote: false,
                })
            }
        }
    }
}

/// A pending request-name completion in the source editor.
pub(crate) struct RequestCompletion {
    /// The dim text shown starting at the cursor.
    pub(crate) ghost: String,
    /// The text inserted at the cursor when the completion is accepted (may add
    /// a closing quote the ghost doesn't show, for the auto-quote case).
    pub(crate) insert: String,
    /// When set, an opening quote is inserted before the current bare name token
    /// on accept (auto-quoting a spaced name).
    pub(crate) wrap_quote: bool,
}

/// Apply `comp` to `ed`: optionally wrap the current bare name token in an
/// opening quote, then insert the completion text at the cursor.
fn accept_request_completion(ed: &mut Editor, comp: &RequestCompletion) {
    ed.clear_selection();
    if comp.wrap_quote {
        // Find the start of the bare token under the cursor (scan left over
        // non-whitespace) and insert the opening quote there.
        let row = ed.row;
        let chars: Vec<char> = ed.lines[row].chars().collect();
        let mut start = ed.col;
        while start > 0 && !chars[start - 1].is_whitespace() && chars[start - 1] != '"' {
            start -= 1;
        }
        let byte = Editor::byte_idx(&ed.lines[row], start);
        ed.lines[row].insert(byte, '"');
        ed.col += 1;
    }
    ed.insert_str(&comp.insert);
}

/// The partially-typed request name on a `REQUEST`/`REPORT REQUEST` line, and
/// whether the author has opened a quote (so a spaced name is being written).
enum NamePartial {
    /// A bare token with no quote and no internal whitespace.
    Bare(String),
    /// The text after an as-yet-unclosed opening quote (may contain spaces).
    Quoted(String),
}

/// Move the editor cursor left to the start of the previous word: skip any
/// whitespace immediately to the left, then the run of non-whitespace. At
/// column 0 this falls back to a plain left move (wrapping to the previous
/// line), matching a normal cursor.
fn word_left(ed: &mut Editor) {
    if ed.col == 0 {
        ed.left();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c > 0 && chars[c - 1].is_whitespace() {
        c -= 1;
    }
    while c > 0 && !chars[c - 1].is_whitespace() {
        c -= 1;
    }
    ed.col = c;
}

/// Move the editor cursor right past the current/next word: skip any whitespace
/// under the cursor, then the run of non-whitespace. At the line end this falls
/// back to a plain right move (wrapping to the next line).
fn word_right(ed: &mut Editor) {
    let len = ed.line_len(ed.row);
    if ed.col >= len {
        ed.right();
        return;
    }
    let chars: Vec<char> = ed.lines[ed.row].chars().collect();
    let mut c = ed.col;
    while c < len && chars[c].is_whitespace() {
        c += 1;
    }
    while c < len && !chars[c].is_whitespace() {
        c += 1;
    }
    ed.col = c;
}

/// If `s` (ignoring case) starts with the keyword `kw` followed by whitespace,
/// return the remainder with that leading whitespace trimmed; else `None`.
fn strip_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let head = s.get(..kw.len())?;
    if head.eq_ignore_ascii_case(kw) {
        let rest = &s[kw.len()..];
        if rest.starts_with(char::is_whitespace) {
            return Some(rest.trim_start());
        }
    }
    None
}

/// Extract the partially-typed request name from a source line, if it is a
/// `REQUEST <name>` or `REPORT REQUEST <name>` line whose name is still being
/// typed (cursor-at-end is checked by the caller). Distinguishes a bare token
/// from an opened-quote fragment so completion can stay grammar-valid.
fn request_name_partial(line: &str) -> Option<NamePartial> {
    let t = line.trim_start();
    let after_report = strip_keyword(t, "REPORT").unwrap_or(t);
    let name = strip_keyword(after_report, "REQUEST")?;
    if let Some(inner) = name.strip_prefix('"') {
        // An opened quote: complete inside it until a closing quote is typed.
        if inner.contains('"') {
            return None; // already closed — the name is finished
        }
        Some(NamePartial::Quoted(inner.to_string()))
    } else if name.is_empty() || name.chars().any(char::is_whitespace) {
        // Empty, or a bare token that already has spaces (unquoted → will fail
        // validation; don't paper over it with a completion).
        None
    } else {
        Some(NamePartial::Bare(name.to_string()))
    }
}

/// Join a `# collection:` reference against the report's own directory (when the
/// ref is relative and the report has a known path), so relative links stay
/// valid regardless of the process's working directory.
fn resolve_ref_path(report_path: Option<&std::path::Path>, cref: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(cref);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(dir) = report_path.and_then(|rp| rp.parent()) {
        return dir.join(p);
    }
    p.to_path_buf()
}

/// Compare two paths, canonicalising when possible (so `./a.hurl` and an
/// absolute form of the same file match) but falling back to a plain equality
/// check for paths that don't yet exist on disk.
fn paths_equal(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Draw a report tab's full-screen body: the binding status, the flow source
/// (syntax-highlighted, editable in place), and the live validation panel.
pub(crate) fn draw_report_body(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    s: &Strings,
    th: &Theme,
) {
    let Some(idx) = app.active_report_index() else {
        return;
    };

    // Number of validation rows to reserve at the bottom (bounded so a long
    // list of problems can't crowd out the source).
    let diag_count = {
        let rt = &app.reports[idx];
        if rt.parse_error.is_some() {
            1
        } else {
            rt.diagnostics.len().max(1)
        }
    };
    let diag_h = (diag_count as u16 + 2).min(10);

    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(diag_h),
    ])
    .split(area);

    draw_report_binding(f, rows[0], app, idx, s, th);
    draw_report_source(f, rows[1], app, idx, s, th);
    draw_report_validation(f, rows[2], app, idx, s, th);
}

/// Render styled `lines` into `block`'s inner area through `panel`, so the read
/// content wraps, scrolls and shows a scrollbar exactly like the collection
/// view's panels.
fn draw_report_panel(
    f: &mut Frame,
    area: Rect,
    block: Block<'static>,
    panel: &mut MultiSelectPanel,
    lines: &[Line<'static>],
    th: &Theme,
) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    panel.set_styled_content(lines, inner.width as usize);
    panel.clamp_scroll(inner.height);
    let visible = panel.visible_rows(inner.height);
    f.render_widget(
        Paragraph::new(visible).style(Style::default().fg(th.text)),
        inner,
    );
    if panel.max_scroll(inner.height) > 0 {
        let total = panel.total_rows().min(u16::MAX as u32) as usize;
        let bar = Rect {
            x: area.x + area.width - 1,
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        draw_scrollbar(
            f,
            bar,
            total,
            inner.height as usize,
            panel.scroll() as usize,
            th,
        );
    }
}

/// Draw the ghost completion `ghost` as dim text starting at the editor's
/// cursor, on top of the already-rendered editor. Mirrors the horizontal /
/// vertical scroll maths [`render_editor_highlighted`] uses so it lands exactly
/// at the cursor cell (the completion is only offered when the cursor is at the
/// end of the line, so it reads as the line's continuation).
fn draw_editor_ghost(f: &mut Frame, area: Rect, ed: &Editor, ghost: &str, th: &Theme) {
    let w = area.width as usize;
    let h = area.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let col_off = ed.col.saturating_sub(w.saturating_sub(1));
    let row_off = ed.row.saturating_sub(h.saturating_sub(1));
    let screen_col = ed.col - col_off;
    let screen_row = ed.row - row_off;
    let avail = w.saturating_sub(screen_col);
    if avail == 0 || screen_row >= h {
        return;
    }
    let shown: String = ghost.chars().take(avail).collect();
    let rect = Rect {
        x: area.x + screen_col as u16,
        y: area.y + screen_row as u16,
        width: shown.chars().count() as u16,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shown,
            Style::default().fg(th.dim).add_modifier(Modifier::DIM),
        ))),
        rect,
    );
}

fn draw_report_binding(
    f: &mut Frame,
    area: Rect,
    app: &TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let rt = &app.reports[idx];
    let line = match app.resolve_bound_collection(&rt.report) {
        Some(ci) => Line::from(vec![
            Span::styled(s.report_bound_prefix, Style::default().fg(th.dim)),
            Span::raw(" "),
            Span::styled(
                app.collections[ci].name.clone(),
                Style::default().fg(th.ok).add_modifier(Modifier::BOLD),
            ),
        ]),
        None => {
            let msg = if rt.report.collection_ref().is_some() {
                s.report_collection_missing
            } else {
                s.report_unbound
            };
            Line::from(Span::styled(msg, Style::default().fg(th.pending)))
        }
    };
    f.render_widget(
        Paragraph::new(line)
            .block(panel(s.report_binding_heading.to_string(), false, th))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_report_source(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    // The tab is named for the report; the source panel advertises only the
    // report-specific action (edit / leave-edit) — the tab-navigation/close
    // shortcuts already live in the footer, so they're not repeated here.
    let editing = app.reports[idx].editor.is_some();
    let hint = if editing {
        s.report_hint_leave
    } else {
        s.report_hint_edit
    };
    let title = format!("{} — {}", s.report_source_heading, hint);
    let block = panel(title, true, th);
    let error_line = app.reports[idx].parse_error_line;

    if editing {
        // Edit focus: render the live editor (with cursor) inside the panel,
        // keeping the same syntax highlighting as the read view.
        let inner = block.inner(area);
        f.render_widget(block, area);
        // A pending `REQUEST`-name completion, drawn dim after the cursor.
        let completion = app.report_request_completion(idx);
        if let Some(editor) = app.reports[idx].editor.as_ref() {
            render_editor_highlighted(f, inner, editor, th, |row, line| {
                super::report_highlight::highlight_row(row, line, error_line, th)
            });
            if let Some(completion) = completion {
                draw_editor_ghost(f, inner, editor, &completion.ghost, th);
            }
        }
        return;
    }

    let body = app.reports[idx].report.text.clone();
    let trimmed = body.trim_end();
    let lines = if trimmed.is_empty() {
        vec![Line::from(Span::styled(
            s.report_empty_source,
            Style::default().fg(th.dim),
        ))]
    } else {
        super::report_highlight::highlight_source(trimmed, error_line, th)
    };
    draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].source_panel,
        &lines,
        th,
    );
}

fn draw_report_validation(
    f: &mut Frame,
    area: Rect,
    app: &mut TuiApp,
    idx: usize,
    s: &Strings,
    th: &Theme,
) {
    let lines: Vec<Line<'static>> = {
        let rt = &app.reports[idx];
        if let Some(err) = &rt.parse_error {
            vec![Line::from(Span::styled(
                err.clone(),
                Style::default().fg(th.err),
            ))]
        } else if rt.diagnostics.is_empty() {
            vec![Line::from(Span::styled(
                s.report_no_diagnostics,
                Style::default().fg(th.ok),
            ))]
        } else {
            rt.diagnostics
                .iter()
                .map(|d| {
                    let (icon, colour) = match d.severity {
                        Severity::Error => ("✗ ", th.err),
                        Severity::Warning => ("! ", th.pending),
                    };
                    Line::from(vec![
                        Span::styled(icon, Style::default().fg(colour)),
                        Span::styled(d.message.clone(), Style::default().fg(th.text)),
                    ])
                })
                .collect()
        }
    };
    let block = panel(s.report_validation_heading.to_string(), false, th);
    draw_report_panel(
        f,
        area,
        block,
        &mut app.reports[idx].validation_panel,
        &lines,
        th,
    );
}
