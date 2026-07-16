//! Panic-safe terminal setup for mouse capture (feature `terminal-guard`).
//!
//! Enabling this crate's panel-scoped drag-selection means turning on the
//! terminal's mouse-tracking mode, so the application — not the terminal
//! emulator — receives mouse events. That global terminal state has to be
//! undone on exit *and* on any panic, or the user's shell is left with mouse
//! tracking still switched on and every subsequent mouse move spews raw
//! tracking escape sequences into the prompt (the terminal appears to fill
//! with garbage).
//!
//! [`TerminalGuard`] centralises that: it enables mouse capture (and,
//! optionally, the keyboard-enhancement protocol), wraps the current panic
//! hook so the state is restored even on an unexpected panic — including a
//! panic raised *inside* a dependency's own event parser — and restores
//! everything again when the guard is dropped on the normal exit path.
//!
//! This lives behind the default-on `terminal-guard` feature so callers who
//! only want the pure selection/wrapping logic can opt out and avoid the
//! process-global panic-hook and terminal side effects entirely.

use std::io;

use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;

/// An RAII guard that turns on terminal mouse capture (and optionally the
/// keyboard-enhancement protocol) for as long as it is alive, and guarantees
/// the terminal is put back the way it was — both on the normal exit path
/// (via [`Drop`]) and on any panic (via a wrapped panic hook).
///
/// Create it *after* your terminal has been put into raw mode / the alternate
/// screen (e.g. after `ratatui::init()`), and drop it *before* you tear that
/// down (e.g. before `ratatui::restore()`):
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use tui_panel_select::TerminalGuard;
///
/// let mut terminal = ratatui::init();
/// let guard = TerminalGuard::install(true)?;
/// let enhanced = guard.keyboard_enhancement_active();
///
/// // ... run your event loop, using `enhanced` to decide whether modifier
/// // combinations like Ctrl+Enter are reported distinctly ...
///
/// drop(guard); // restore mouse capture / keyboard flags
/// ratatui::restore();
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct TerminalGuard {
    keyboard_enhancement_active: bool,
}

impl TerminalGuard {
    /// Enable mouse capture and install the panic-safe restore hook.
    ///
    /// When `keyboard_enhancement` is `true` and the terminal advertises
    /// support, the keyboard-enhancement (disambiguate-escape-codes) protocol
    /// is pushed as well, so modifier combinations such as Ctrl+Enter are
    /// reported distinctly from a plain Enter. Use
    /// [`keyboard_enhancement_active`](Self::keyboard_enhancement_active) to
    /// learn whether it actually took effect.
    ///
    /// The current panic hook is taken and wrapped: on any panic the wrapper
    /// disables mouse capture (and pops the keyboard-enhancement flags, if
    /// they were pushed) before delegating to the previous hook.
    pub fn install(keyboard_enhancement: bool) -> io::Result<Self> {
        let enhanced = keyboard_enhancement && supports_keyboard_enhancement().unwrap_or(false);
        if enhanced {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            )?;
        }
        execute!(io::stdout(), EnableMouseCapture)?;

        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            if enhanced {
                let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
            }
            previous_hook(info);
        }));

        Ok(Self {
            keyboard_enhancement_active: enhanced,
        })
    }

    /// Whether the keyboard-enhancement protocol was actually enabled (it is
    /// only enabled when requested *and* supported by the terminal).
    pub fn keyboard_enhancement_active(&self) -> bool {
        self.keyboard_enhancement_active
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.keyboard_enhancement_active {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
}
