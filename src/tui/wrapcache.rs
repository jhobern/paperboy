//! Re-export of the panel line/wrap cache types from the `tui-panel-select`
//! crate.
//!
//! The wrap/selection implementation lives in that crate so it can be reused
//! across TUI apps; PaperBoy now drives it through `MultiSelectPanel`, but
//! still consumes the `TextPos` coordinate type here under the original
//! module path so callers need no import changes.

pub(crate) use tui_panel_select::wrapcache::TextPos;
