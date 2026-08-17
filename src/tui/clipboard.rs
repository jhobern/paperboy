//! The single funnel for clipboard copies, wrapping the helper from the
//! `tui-panel-select` crate (a local clipboard tool when available, else an
//! OSC 52 escape sequence).
//!
//! The implementation lives in that crate so it can be reused across TUI
//! apps; PaperBoy consumes it here under the original module path.
//!
//! Everything that copies must go through *this* wrapper rather than calling
//! the crate directly, because of the `cfg(test)` guard below.

/// Copy `text` to the system clipboard, best-effort.
///
/// Under `cargo test` this is pinned to [`ClipboardMode::None`] so the suite
/// cannot touch the developer's real desktop clipboard. The upstream crate
/// cannot do this for us: its own `cfg(test)` is only set while *it* is being
/// tested, and is inert when it is built as our dependency. Our `cfg(test)`,
/// on the other hand, is set precisely when we want it to be.
///
/// The mode is pinned here, on every call, rather than once during test setup
/// because there is no single entry point every test passes through — doing it
/// at the funnel means a new test cannot forget.
///
/// [`ClipboardMode::None`]: tui_panel_select::ClipboardMode::None
pub(crate) fn copy_to_clipboard(text: &str) {
    #[cfg(test)]
    tui_panel_select::set_clipboard_mode(tui_panel_select::ClipboardMode::None);
    tui_panel_select::clipboard::copy_to_clipboard(text);
}

#[cfg(test)]
mod tests {
    /// Guards the guard. If this ever fails, the whole suite has started
    /// overwriting whatever the developer had on their clipboard, and
    /// spawning a helper process per copy along with it.
    #[test]
    fn copying_never_reaches_a_real_clipboard_under_test() {
        super::copy_to_clipboard("paperboy test must not reach a real clipboard");
        assert_eq!(
            tui_panel_select::clipboard::clipboard_mode(),
            tui_panel_select::ClipboardMode::None,
        );
    }
}
