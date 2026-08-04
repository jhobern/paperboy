//! The theme model now lives in the front-end-agnostic [`crate::theme`] module
//! so both the terminal UI and the GUI share one definition (colours, presets,
//! language mapping) without duplication. This re-export keeps the terminal
//! UI's existing `super::theme::*` / `crate::tui::theme::*` paths working.

pub(crate) use crate::theme::*;
