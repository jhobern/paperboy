//! Terminal UI (ratatui) mirroring the GUI: same panels, endpoints, collections,
//! environments, i18n and theming, driven by the keyboard instead of the mouse.
//!
//! Launched with `-r` / `--ratatui`.

mod app;
mod clipboard;
mod draw;
mod editor;
mod git_save;
mod input;
mod line_editor;
mod new_request;
mod postman;
pub(crate) mod remote;
// Shared with the GUI (`gui::report_editor`'s Source view) so a PaperTrail
// script is coloured identically in both front-ends: it produces `ratatui`
// spans, which the GUI converts to egui text sections rather than reimplementing
// the rules and letting the two drift.
pub(crate) mod report_highlight;
mod report_nodes;
mod reports;
#[cfg(test)]
mod tests;
pub(crate) mod theme;
mod theme_editor;

use ratatui::crossterm::event::{
    self, Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use std::io;
use std::time::Duration;

use app::TuiApp;
use draw::draw;
use tui_panel_select::TerminalGuard;

/// Entry point: set up the terminal, run the loop, and restore on exit.
pub fn run() -> io::Result<()> {
    let mut terminal = ratatui::init();

    // Enable mouse capture (so drag selections can be scoped to a single panel
    // instead of the terminal emulator's own whole-row native selection) and
    // the keyboard-enhancement protocol where supported (so Ctrl+Enter is
    // reported distinctly from a plain Enter; F5 is the universal fallback).
    // The guard also wraps the panic hook so both are restored on any panic —
    // including a panic inside crossterm's own SGR mouse-sequence parser —
    // instead of leaving the shell with mouse tracking still switched on and
    // filling with garbage. See the `tui-panel-select` `terminal` module.
    let guard = TerminalGuard::install(true)?;
    let enhanced = guard.keyboard_enhancement_active();

    let mut app = TuiApp::restored();
    app.enhanced_keys = enhanced;
    let result = loop {
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e);
        }
        // Apply any background secret-resolution results (non-blocking).
        app.poll_env_updates();
        // Apply completed response captures so later requests can use them.
        app.poll_capture_updates();
        // Advance the remote-git wizard when a background op finishes.
        app.poll_git_updates();
        // Advance the Postman bulk-import wizard when its worker reports in.
        app.poll_postman_updates();
        // Advance a background Workspace-redownload attempt (see
        // `Overlay::WorkspaceReloadConfirm`/`WorkspaceReloadLoading`).
        app.poll_workspace_redownload_updates();
        // Advance the "save to git" wizard when a background op finishes.
        app.poll_git_save_updates();
        // Apply completed "Run All" (Alt+F5) results (captures + pass/fail markers).
        app.poll_batch_run_updates();
        // Apply a finished background report run (non-blocking; the app stayed
        // responsive while it ran).
        app.poll_report_run_updates();
        match event::poll(Duration::from_millis(120)) {
            Ok(true) => {
                // Terminal (column, row) bounds to clamp every incoming
                // mouse event against — some terminals report stale or
                // out-of-range coordinates once the mouse leaves the window
                // (or during a fast drag past its edge), and every part of
                // the app assumes a mouse point is within the actual
                // rendered area. Clamping here, once, keeps that invariant
                // true everywhere downstream instead of each call site
                // having to re-guard against it.
                let bounds = terminal.size().unwrap_or_default();
                let clamp_mouse = |mut m: MouseEvent| {
                    if bounds.width > 0 {
                        m.column = m.column.min(bounds.width - 1);
                    }
                    if bounds.height > 0 {
                        m.row = m.row.min(bounds.height - 1);
                    }
                    m
                };
                // Drain every event already queued (not just the one that
                // woke us) before drawing again — a fast paste arrives as a
                // burst of individual Key events, and a mouse drag can queue
                // up many Drag events between frames. Applying them all
                // before a single redraw (instead of one redraw per event)
                // is what keeps paste/drag responsive instead of appearing
                // to advance at ~1 event per frame. Capped so an unusually
                // fast/continuous stream of input (e.g. a mouse reporting
                // motion at a very high rate) can never starve the redraw
                // entirely — the screen must still update periodically even
                // mid-flood.
                let mut pending_drag: Option<MouseEvent> = None;
                let mut err = None;
                let mut drained = 0u32;
                const MAX_DRAINED_PER_FRAME: u32 = 512;
                loop {
                    match event::read() {
                        Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                            if let Some(drag) = pending_drag.take() {
                                app.on_mouse(drag);
                            }
                            app.on_key(key);
                        }
                        Ok(Event::Mouse(mouse)) => {
                            let mouse = clamp_mouse(mouse);
                            // Consecutive Drag events collapse into just the
                            // latest position — only the final position of a
                            // burst of mouse-moves matters for a highlight
                            // recompute, so this turns N queued drags into a
                            // single selection update.
                            if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
                                pending_drag = Some(mouse);
                            } else {
                                if let Some(drag) = pending_drag.take() {
                                    app.on_mouse(drag);
                                }
                                app.on_mouse(mouse);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                    drained += 1;
                    if drained >= MAX_DRAINED_PER_FRAME {
                        break;
                    }
                    match event::poll(Duration::ZERO) {
                        Ok(true) => continue,
                        Ok(false) => break,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(drag) = pending_drag.take() {
                    app.on_mouse(drag);
                }
                if let Some(e) = err {
                    break Err(e);
                }
            }
            Ok(false) => {}
            Err(e) => break Err(e),
        }
        // Keep auto-scrolling a selection drag held past its panel's edge
        // even when the mouse itself isn't moving (no new Drag event to
        // drive it) — ticked once per idle loop iteration, roughly every
        // 120ms while nothing else arrives.
        if app.has_pending_autoscroll() {
            app.autoscroll_tick();
        }
        if app.quit {
            app.save_state();
            break Ok(());
        }
    };

    drop(guard); // pops keyboard-enhancement flags + disables mouse capture
    ratatui::restore();
    result
}
