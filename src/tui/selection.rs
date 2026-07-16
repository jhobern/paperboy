//! Re-export of the panel-scoped text-selection primitives from the
//! `tui-panel-select` crate (`point_to_textpos`, `extract_text`,
//! `highlight_cells`, `strip_positions`, `ordered`).
//!
//! The implementation lives in that crate so it can be reused across TUI
//! apps; PaperBoy consumes it here under the original module path.

pub(crate) use tui_panel_select::selection::*;
