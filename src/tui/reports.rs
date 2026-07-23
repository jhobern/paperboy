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
use ratatui::widgets::{Paragraph, Wrap};

use super::app::{Overlay, Pane, TuiApp};
use super::draw::panel;
use super::editor::{Editor, render_editor};
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
    /// When `Some`, the source panel has *edit focus*: keystrokes type into
    /// this live buffer (mirrored into `report.text` on every edit so the
    /// validation panel and tab name stay current) instead of acting as view
    /// shortcuts. `None` = navigation mode.
    pub(crate) editor: Option<Editor>,
}

impl ReportTab {
    pub(crate) fn new(report: Report) -> Self {
        Self {
            report,
            diagnostics: Vec::new(),
            parse_error: None,
            editor: None,
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
        let (parse_error, diagnostics) = {
            let Some(rt) = self.reports.get(idx) else {
                return;
            };
            match rt.report.flow() {
                Err(e) => (Some(e.to_string()), Vec::new()),
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
                    (None, validate(&flow, &ctx))
                }
            }
        };
        if let Some(rt) = self.reports.get_mut(idx) {
            rt.parse_error = parse_error;
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
            .push(super::app::ClosedTab::Report(ridx, rt));
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
            _ => {}
        }
    }

    /// Key handling while the active report's source panel has edit focus.
    /// Esc leaves edit focus (keeping the typed text — edits are applied live);
    /// everything else edits the multiline buffer (Enter inserts a newline).
    /// After each edit the buffer is mirrored into `report.text` and the tab is
    /// revalidated so the validation panel stays live.
    fn on_key_report_editing(&mut self, key: KeyEvent, idx: usize) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
            let mut changed = false;
            match key.code {
                KeyCode::Esc => leave = true,
                KeyCode::Enter => {
                    editor.newline();
                    changed = true;
                }
                KeyCode::Char('y') if ctrl => copy = editor.selected_text(),
                KeyCode::Char(c) => {
                    editor.clear_selection();
                    editor.insert(c);
                    changed = true;
                }
                KeyCode::Backspace => {
                    editor.clear_selection();
                    editor.backspace();
                    changed = true;
                }
                KeyCode::Left => {
                    editor.set_selecting(shift);
                    if ctrl { editor.home() } else { editor.left() }
                }
                KeyCode::Right => {
                    editor.set_selecting(shift);
                    if ctrl { editor.end() } else { editor.right() }
                }
                KeyCode::Up => {
                    editor.set_selecting(shift);
                    editor.up();
                }
                KeyCode::Down => {
                    editor.set_selecting(shift);
                    editor.down();
                }
                KeyCode::Home => {
                    editor.clear_selection();
                    editor.home();
                }
                KeyCode::End => {
                    editor.clear_selection();
                    editor.end();
                }
                _ => {}
            }
            if leave || changed {
                Some(editor.text())
            } else {
                None
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

/// Draw a report tab's full-screen body: the binding status, the (read-only)
/// flow source, and the live validation panel.
pub(crate) fn draw_report_body(f: &mut Frame, area: Rect, app: &TuiApp, s: &Strings, th: &Theme) {
    let Some(rt) = app.active_report() else {
        return;
    };

    // Number of validation rows to reserve at the bottom (bounded so a long
    // list of problems can't crowd out the source).
    let diag_count = if rt.parse_error.is_some() {
        1
    } else {
        rt.diagnostics.len().max(1)
    };
    let diag_h = (diag_count as u16 + 2).min(10);

    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(diag_h),
    ])
    .split(area);

    draw_report_binding(f, rows[0], app, rt, s, th);
    draw_report_source(f, rows[1], rt, s, th);
    draw_report_validation(f, rows[2], rt, s, th);
}

fn draw_report_binding(
    f: &mut Frame,
    area: Rect,
    app: &TuiApp,
    rt: &ReportTab,
    s: &Strings,
    th: &Theme,
) {
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

fn draw_report_source(f: &mut Frame, area: Rect, rt: &ReportTab, s: &Strings, th: &Theme) {
    // The tab is named for the report; the source panel advertises only the
    // report-specific action (edit / leave-edit) — the tab-navigation/close
    // shortcuts already live in the footer, so they're not repeated here.
    let editing = rt.editor.is_some();
    let hint = if editing {
        s.report_hint_leave
    } else {
        s.report_hint_edit
    };
    let title = format!("{} — {}", s.report_source_heading, hint);
    let block = panel(title, true, th);
    if let Some(editor) = &rt.editor {
        // Edit focus: render the live editor (with cursor) inside the panel.
        let inner = block.inner(area);
        f.render_widget(block, area);
        render_editor(f, inner, editor, false, th);
    } else {
        let body = rt.report.text.trim_end();
        let para = if body.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                s.report_empty_source,
                Style::default().fg(th.dim),
            )))
        } else {
            Paragraph::new(body.to_string()).style(Style::default().fg(th.text))
        };
        f.render_widget(para.block(block), area);
    }
}

fn draw_report_validation(f: &mut Frame, area: Rect, rt: &ReportTab, s: &Strings, th: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(err) = &rt.parse_error {
        lines.push(Line::from(Span::styled(
            err.clone(),
            Style::default().fg(th.err),
        )));
    } else if rt.diagnostics.is_empty() {
        lines.push(Line::from(Span::styled(
            s.report_no_diagnostics,
            Style::default().fg(th.ok),
        )));
    } else {
        for d in &rt.diagnostics {
            let (icon, colour) = match d.severity {
                Severity::Error => ("✗ ", th.err),
                Severity::Warning => ("! ", th.pending),
            };
            lines.push(Line::from(vec![
                Span::styled(icon, Style::default().fg(colour)),
                Span::styled(d.message.clone(), Style::default().fg(th.text)),
            ]));
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(panel(s.report_validation_heading.to_string(), false, th))
            .wrap(Wrap { trim: true }),
        area,
    );
}
