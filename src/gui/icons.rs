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
/// A PaperTrail report file (`.trail`) in the tree. A chart, deliberately
/// unlike the collection's document glyph — a report *produces* a table, it
/// isn't another list of requests.
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
/// Save action (report editor Save button).
pub const SAVE: &str = p::FLOPPY_DISK;
/// Move a selected report block up in its list.
pub const CARET_UP: &str = p::CARET_UP;
/// Delete a selected report block.
pub const TRASH: &str = p::TRASH;
/// Stop an in-flight report run.
pub const STOP: &str = p::STOP;
/// Export a report's results to a file.
pub const EXPORT: &str = p::EXPORT;
/// A row queued in the streaming results grid (formerly `·`).
pub const ROW_SCHEDULED: &str = p::DOT_OUTLINE;
/// Preview a report's projected rows without sending anything (the dry run).
/// An eye rather than a second play glyph, so it can't be mistaken for Run.
pub const PREVIEW: &str = p::EYE;
/// A request / collection edited since it was last read from (or written to)
/// disk. A pencil rather than the conventional `*` or `●`, because the marker
/// shares the row's right-hand gutter with the pass/fail run marks and a dot
/// there reads as "queued" (see [`ROW_SCHEDULED`]).
pub const EDITED: &str = p::PENCIL_SIMPLE;
