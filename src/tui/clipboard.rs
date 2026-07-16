//! Re-export of the clipboard-copy helper from the `tui-panel-select` crate
//! (a local clipboard tool when available, else an OSC 52 escape sequence).
//!
//! The implementation lives in that crate so it can be reused across TUI
//! apps; PaperBoy consumes it here under the original module path.

pub(crate) use tui_panel_select::clipboard::copy_to_clipboard;
