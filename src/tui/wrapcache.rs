//! Re-export of the panel line/wrap cache from the `tui-panel-select` crate.
//!
//! The implementation lives in that crate so it can be reused across TUI
//! apps; PaperBoy consumes it here under the original module path so the
//! rest of the code (and its tests) need no import changes.

pub(crate) use tui_panel_select::wrapcache::{PanelWrap, TextPos};
