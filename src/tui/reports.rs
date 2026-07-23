//! The TUI-side wrapper around a PaperTrail [`Report`] plus the "Reports view"
//! — a full-screen tab kind that lives in the same tab strip as the collection
//! tabs but shows only report content (no environments / response / raw-view
//! panels, so it fits small monitors, per the design).
//!
//! The core [`Report`] stays front-end agnostic; everything ratatui-specific
//! (cached diagnostics, the modal source editor, drawing) lives here so a
//! future GUI can reuse the core unchanged. A report tab is a mostly-read-only
//! view of the flow source plus its live validation; editing happens in a modal
//! [`Overlay::ReportEdit`] editor (like Raw Mode for a request) so single-key
//! shortcuts in the main view never collide with typing into the source.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::app::{Overlay, Pane, TuiApp};
use super::draw::{centered_rect, panel};
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
}

impl ReportTab {
    pub(crate) fn new(report: Report) -> Self {
        Self {
            report,
            diagnostics: Vec::new(),
            parse_error: None,
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

    /// Open the modal source editor for the active report tab.
    pub(crate) fn open_report_editor(&mut self) {
        if let Some(rt) = self.active_report() {
            let editor = Editor::new(&rt.report.text, true);
            self.overlay = Some(Overlay::ReportEdit { editor });
        }
    }

    /// Commit edited source text back into the active report, refresh its name,
    /// revalidate and persist. Called when the modal editor is confirmed.
    pub(crate) fn commit_report_editor(&mut self, text: String) {
        if let Some(idx) = self.active_report_index() {
            if let Some(rt) = self.reports.get_mut(idx) {
                rt.report.set_text(text);
            }
            self.revalidate_report(idx);
            self.save_state();
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
            // Report-specific: edit the source.
            KeyCode::Char('e') => self.open_report_editor(),
            _ => {}
        }
    }

    /// Key handling for the modal report-source editor ([`Overlay::ReportEdit`]).
    /// F2 / Ctrl+S commit the edited text (and revalidate); Esc cancels.
    /// Everything else edits the multiline buffer (Enter inserts a newline).
    pub(crate) fn report_edit_key_handler(&mut self, key: KeyEvent, mut editor: Editor) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        enum Act {
            Commit,
            Cancel,
            Edit,
        }
        let act = match key.code {
            KeyCode::Esc => Act::Cancel,
            KeyCode::F(2) => Act::Commit,
            KeyCode::Char('s') if ctrl => Act::Commit,
            KeyCode::Enter => {
                editor.newline();
                Act::Edit
            }
            KeyCode::Char('y') if ctrl => {
                if let Some(text) = editor.selected_text() {
                    super::clipboard::copy_to_clipboard(&text);
                    self.status = Some(crate::i18n::Status::Copied);
                }
                Act::Edit
            }
            KeyCode::Char(c) => {
                editor.clear_selection();
                editor.insert(c);
                Act::Edit
            }
            KeyCode::Backspace => {
                editor.clear_selection();
                editor.backspace();
                Act::Edit
            }
            KeyCode::Left => {
                editor.set_selecting(shift);
                if ctrl {
                    editor.home()
                } else {
                    editor.left()
                }
                Act::Edit
            }
            KeyCode::Right => {
                editor.set_selecting(shift);
                if ctrl {
                    editor.end()
                } else {
                    editor.right()
                }
                Act::Edit
            }
            KeyCode::Up => {
                editor.set_selecting(shift);
                editor.up();
                Act::Edit
            }
            KeyCode::Down => {
                editor.set_selecting(shift);
                editor.down();
                Act::Edit
            }
            KeyCode::Home => {
                editor.clear_selection();
                editor.home();
                Act::Edit
            }
            KeyCode::End => {
                editor.clear_selection();
                editor.end();
                Act::Edit
            }
            _ => Act::Edit,
        };
        match act {
            Act::Commit => self.commit_report_editor(editor.text()),
            Act::Cancel => {}
            Act::Edit => self.overlay = Some(Overlay::ReportEdit { editor }),
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
    // The tab is named for the report; the source panel carries the primary
    // hints so the (otherwise chrome-free) view still advertises its keys.
    let title = format!("{} — {}", s.report_source_heading, s.report_hints);
    let body = rt.report.text.trim_end();
    let para = if body.is_empty() {
        Paragraph::new(Line::from(Span::styled(
            s.report_empty_source,
            Style::default().fg(th.dim),
        )))
    } else {
        Paragraph::new(body.to_string()).style(Style::default().fg(th.text))
    };
    f.render_widget(para.block(panel(title, true, th)), area);
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

/// Draw the modal report-source editor overlay ([`Overlay::ReportEdit`]).
pub(crate) fn draw_report_edit_overlay(f: &mut Frame, editor: &Editor, s: &Strings, th: &Theme) {
    let full = f.area();
    let w = full.width.saturating_sub(8).max(20);
    let h = full.height.saturating_sub(6).max(6);
    let area = centered_rect(w, h, full);
    f.render_widget(Clear, area);
    let title = format!("{}  ({})", s.report_edit_title, s.prompt_save_hint_ml);
    let block = panel(title, true, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    render_editor(f, inner, editor, false, th);
}
