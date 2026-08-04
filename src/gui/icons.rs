//! GUI icon glyphs, drawn from the Phosphor icon font (registered in
//! [`super::app::GuiApp::new`] via `egui_phosphor::add_to_fonts`).
//!
//! egui's bundled fonts (Ubuntu-Light / NotoEmoji / emoji-icon-font) do **not**
//! actually contain the emoji and symbols the tree and buttons want — `📁`,
//! `📄`, `📊`, `🔑`, the `▾`/`▸` chevrons and the `✓`/`✗`/`✕`/`＋` marks all
//! report `has_glyph == false` and render as empty "tofu" boxes on the target
//! systems. Phosphor's glyphs live in the Private Use Area and render reliably
//! in the proportional family every label uses, so every GUI icon is sourced
//! from here rather than from a bare Unicode literal.
//!
//! These are thin, readable aliases over the `egui_phosphor::regular`
//! constants so call sites read `icons::FOLDER` instead of a bare escape, and a
//! future weight/variant swap is a one-file change.

use egui_phosphor::regular as p;

/// Collapsed tree row (folder/collection closed) — leading disclosure caret.
pub const CARET_RIGHT: &str = p::CARET_RIGHT;
/// Expanded tree row (folder/collection open) — leading disclosure caret.
pub const CARET_DOWN: &str = p::CARET_DOWN;

/// A folder in the workspace/collection tree.
pub const FOLDER: &str = p::FOLDER;
/// A collection file (`.hurl` / `.json`) in the tree.
pub const FILE: &str = p::FILE_TEXT;
/// A PaperTrail report file (`.trail`) in the tree.
pub const REPORT: &str = p::CHART_BAR;
/// An environment file (`.vars`) in the tree, and the git-loaded env marker.
pub const ENV: &str = p::KEY;

/// Run / send action (formerly `▶`).
pub const PLAY: &str = p::PLAY;
/// Add action (formerly `＋`).
pub const PLUS: &str = p::PLUS;
/// Close / remove action (formerly `✕`).
pub const CLOSE: &str = p::X;
/// A passing run or assertion (formerly `✓`).
pub const PASS: &str = p::CHECK;
/// A failing run or assertion (formerly `✗`).
pub const FAIL: &str = p::X;
/// A request or run in progress (formerly `…`).
pub const RUNNING: &str = p::CIRCLE_NOTCH;
/// Warning / error banner marker (formerly `⚠`).
pub const WARNING: &str = p::WARNING;
/// A git-remote-linked collection or environment (formerly `⎇`).
pub const GIT: &str = p::GIT_BRANCH;
/// The "active" marker on the linked Global Environment (formerly `●`).
pub const ACTIVE: &str = p::DOT;
