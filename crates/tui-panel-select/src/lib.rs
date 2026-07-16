//! Panel-scoped text selection and clipboard copy for [ratatui] apps.
//!
//! A terminal's own click-drag selection can't be confined to one panel — it
//! spans the full terminal row, sweeping up borders and neighbouring panels.
//! This crate lets an app capture the mouse itself and implement its own
//! selection that is **confined to a single panel's rectangle**, uses natural
//! "stream" semantics (never a rectangular block), **survives resizes,
//! rewraps and scrolling** (selections are stored as logical line/column
//! positions, not stale screen cells), and stays cheap even for
//! multi-megabyte content (only what's on screen is ever wrapped or painted).
//! On mouse-up the selected text is copied to the system clipboard, working
//! both on a local desktop and over SSH/tmux (OSC 52 fallback).
//!
//! # Two ways to use it
//!
//! **Batteries-included:** [`SelectablePanel`] bundles the cache and
//! selection state into one object with a tiny API — `set_content`,
//! `begin_selection`, `extend_selection`, `selected_text`, `copy_selection`,
//! `highlight_cells`, `visible_rows`. See its module for a worked example.
//!
//! **Primitives:** if your app already owns its selection state (e.g. you
//! support multiple simultaneous selections or keyboard extension), use the
//! stateless building blocks directly:
//! - [`PanelWrap`] / [`TextPos`] — the line/wrap cache and logical positions
//!   ([`wrapcache`]).
//! - [`selection`] — pure functions: `point_to_textpos`, `extract_text`,
//!   `highlight_cells`, `strip_positions`.
//! - [`clipboard`] — `copy_to_clipboard` (local tool + OSC 52 fallback).
//! - [`wrap`] — the underlying character-exact line-wrapping helpers.
//!
//! [ratatui]: https://docs.rs/ratatui

pub mod clipboard;
pub mod panel;
pub mod selection;
#[cfg(feature = "terminal-guard")]
pub mod terminal;
pub mod wrap;
pub mod wrapcache;

pub use panel::{MouseAction, MouseConfig, SelectablePanel};
#[cfg(feature = "terminal-guard")]
pub use terminal::TerminalGuard;
pub use wrapcache::{PanelWrap, TextPos};
