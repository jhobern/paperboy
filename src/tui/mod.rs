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
mod new_request;
pub(crate) mod remote;
mod selection;
#[cfg(test)]
mod tests;
pub(crate) mod theme;
mod theme_editor;
mod wrapcache;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use std::io;
use std::time::Duration;

use app::TuiApp;
use draw::draw;

/// Entry point: set up the terminal, run the loop, and restore on exit.
pub fn run() -> io::Result<()> {
    let mut terminal = ratatui::init();

    // Enable the keyboard enhancement protocol where the terminal supports it,
    // so modifier combinations like Ctrl+Enter are reported distinctly from a
    // plain Enter. (F5 is the universal fallback for terminals that don't.)
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        );
    }
    // Capture the mouse ourselves so drag selections can be scoped to a
    // single panel (Request JSON / Response) instead of the terminal
    // emulator's own whole-row native selection.
    let _ = execute!(io::stdout(), EnableMouseCapture);

    // `ratatui::init()` already installed a panic hook that disables raw
    // mode and leaves the alternate screen on any panic — but it has no
    // idea we also turned on mouse capture (and, maybe, the keyboard
    // enhancement protocol) above, so it never undoes *those*. If anything
    // panics while either is still active — including a panic inside
    // crossterm's own event parser itself (a real, reproducible crash: a
    // malformed/edge-case SGR mouse escape sequence, e.g. one reporting a
    // coordinate of 0, hits an unchecked `- 1` in
    // crossterm 0.29.0's `parse_csi_sgr_mouse` and panics) — the terminal
    // emulator is left with mouse tracking still switched on. Every mouse
    // move after that keeps sending raw tracking escape sequences straight
    // into what is now just a plain shell prompt, which shows up to the
    // user as the terminal continuously filling with garbage characters:
    // exactly this bug's reported symptom. Wrapping the existing hook with
    // one that also disables mouse capture (and pops the keyboard
    // enhancement flags, if pushed) closes that gap for *any* panic,
    // regardless of whether it originates in our own code or a dependency.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        if enhanced {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        previous_hook(info);
    }));

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
        // Advance a background Workspace-redownload attempt (see
        // `Overlay::WorkspaceReloadConfirm`/`WorkspaceReloadLoading`).
        app.poll_workspace_redownload_updates();
        // Advance the "save to git" wizard when a background op finishes.
        app.poll_git_save_updates();
        // Apply completed "Run All" (Alt+F5) results (captures + pass/fail markers).
        app.poll_batch_run_updates();
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
        if app.pending_autoscroll.is_some() {
            app.autoscroll_tick();
        }
        if app.quit {
            app.save_state();
            break Ok(());
        }
    };

    if enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
