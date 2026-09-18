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
//! These are thin, readable aliases over the Phosphor constants so call sites
//! read `icons::FOLDER` instead of a bare escape, and a future weight/variant
//! swap is a one-file change.
//!
//! The weight is chosen in [`super::app::GuiApp::new`], not here: every Phosphor
//! variant maps the *same* codepoints (`FOLDER` is `U+E24A` in all of them), so
//! these constants are identical whichever weight is registered, and only one
//! variant can be registered at a time — `add_to_fonts` always inserts under
//! the font key `"phosphor"`, so a second call would replace the first rather
//! than sit beside it. Mixing two icon weights in one build would therefore
//! mean registering a second family by hand and paying for a second copy of the
//! font; it isn't worth it for a weight change.

use egui_phosphor::light as p;
use std::collections::HashMap;

/// Collapsed tree row (folder/collection closed) — leading disclosure caret.
pub const CARET_RIGHT: &str = p::CARET_RIGHT;
/// Expanded tree row (folder/collection open) — leading disclosure caret.
pub const CARET_DOWN: &str = p::CARET_DOWN;

/// The Source view's find bar.
pub const SEARCH: &str = p::MAGNIFYING_GLASS;

/// The Environments panel's "go to the active environment" button.
pub const GOTO_ACTIVE: &str = p::CROSSHAIR_SIMPLE;

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

/// The requests list is in the collection file's own order (the default).
pub const SORT_FILE: &str = p::LIST;
/// The requests list is sorted A-Z.
pub const SORT_ASC: &str = p::SORT_ASCENDING;
/// The requests list is sorted Z-A.
pub const SORT_DESC: &str = p::SORT_DESCENDING;

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
/// Hand an exported file to the desktop's default application (it leaves the
/// app, hence the arrow out of a box rather than a second export glyph).
pub const OPEN_EXTERNAL: &str = p::ARROW_SQUARE_OUT;
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
/// Stands in for a newline that was collapsed to fit a value onto one row of
/// the report results grid. The obvious `⏎` (U+23CE) is a tofu box here — no
/// font egui bundles carries it, and unlike the dots in [`super::widgets`]
/// this one sits *inside* a run of text, so it can't be painted around. The
/// terminal UI keeps `⏎`, which a terminal font does have.
pub const CELL_NEWLINE: &str = p::ARROW_ELBOW_DOWN_LEFT;

/// Directional arrows, for the shared strings that spell a menu path or a key
/// hint with one (see [`SUBSTITUTIONS`]).
pub const ARROW_LEFT: &str = p::ARROW_LEFT;
/// See [`ARROW_LEFT`].
pub const ARROW_RIGHT: &str = p::ARROW_RIGHT;
/// See [`ARROW_LEFT`].
pub const ARROW_UP: &str = p::ARROW_UP;
/// See [`ARROW_LEFT`].
pub const ARROW_DOWN: &str = p::ARROW_DOWN;

/// Characters that a shared [`crate::i18n`] string may legitimately contain,
/// but that the GUI's font stack cannot draw, paired with something that says
/// the same thing and that it can.
///
/// The string table serves both front-ends, and a terminal's font is far
/// better stocked than the three fonts egui bundles: `⚠`, `▶`, `＋` and the
/// arrows are perfectly ordinary in the terminal UI and tofu boxes here.
/// Rather than flatten the table to the poorer of the two alphabets — which
/// would make every warning and every key hint worse in the front-end that
/// renders them properly — the GUI swaps in an icon as it reads them.
///
/// This maps by **meaning, not by shape**: `▶ Run` becomes the same play icon
/// the GUI's own Run button uses, and `⏳ Running…` the same spinner its
/// report toolbar uses, so a shared string arrives in the icon vocabulary the
/// surrounding GUI is already speaking. The one exception is `●`, which has no
/// counterpart — Phosphor has no solid disc at text size, which is why
/// [`super::widgets::status_dot`] paints one rather than typing it, and a dot
/// inside a run of text cannot be painted around. It falls back to the smaller
/// `•`, which the bundled fonts do carry.
///
/// Rows are listed here whether or not the GUI has a use for them today. A row
/// that only the terminal UI shows costs nothing to cover, and covering it
/// means the string is already safe on the day someone shows it in the GUI —
/// which is the day nobody would think to check.
const SUBSTITUTIONS: &[(char, &str)] = &[
    ('\u{2190}', ARROW_LEFT),
    ('\u{2191}', ARROW_UP),
    ('\u{2192}', ARROW_RIGHT),
    ('\u{2193}', ARROW_DOWN),
    ('\u{23f3}', RUNNING),
    ('\u{25b6}', PLAY),
    ('\u{25cf}', "\u{2022}"),
    ('\u{26a0}', WARNING),
    ('\u{270e}', EDITED),
    ('\u{2716}', CLOSE),
    ('\u{ff0b}', PLUS),
];

/// `text` with every character the GUI cannot draw replaced by an icon that
/// can be (see [`SUBSTITUTIONS`]).
///
/// Borrowed rather than owned on the overwhelmingly common path: all but a
/// handful of rows come back untouched, and the ones that don't are rewritten
/// at most once each per process, so this allocates a few dozen bytes in total
/// and then never again. That is what buys the `&'static str` the string table
/// is built from, and with it the guarantee that *every* GUI string has been
/// through here — a substitution applied at call sites instead would only ever
/// cover the call sites somebody remembered.
pub fn drawable(text: &'static str) -> &'static str {
    if !text.contains(|c| SUBSTITUTIONS.iter().any(|(bad, _)| c == *bad)) {
        return text;
    }
    static FIXED: std::sync::OnceLock<std::sync::Mutex<HashMap<&'static str, &'static str>>> =
        std::sync::OnceLock::new();
    let mut fixed = FIXED
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .expect("the substitution cache is only ever held for a map lookup");
    fixed.entry(text).or_insert_with(|| {
        let mut out = text.to_owned();
        for (bad, icon) in SUBSTITUTIONS {
            out = out.replace(*bad, icon);
        }
        // Leaked deliberately: the result has to outlive every `Strings` built
        // from it, the set of inputs is the fixed string table, and the cache
        // above means each one is built once. Bounded, tiny, and never grows.
        Box::leak(out.into_boxed_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    /// Lay a string out the way the GUI does and report the characters the
    /// font stack has no glyph for. egui substitutes a "tofu" replacement
    /// glyph silently: the code reads correctly, the layout comes out the
    /// right width, and the only way to find out is to look at the screen.
    fn undrawable(ctx: &egui::Context, text: &str) -> Vec<char> {
        ctx.fonts_mut(|f| {
            let id = egui::FontId::new(14.0, egui::FontFamily::Proportional);
            text.chars()
                .filter(|c| !c.is_control() && !f.has_glyph(&id, *c))
                .collect()
        })
    }

    /// The GUI's real font stack: egui's bundled fonts plus Phosphor, exactly
    /// as [`super::super::app::GuiApp::new`] registers them.
    fn gui_fonts() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Light);
        ctx.set_fonts(fonts);
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        ctx
    }

    /// The guard this module exists for. Three separate releases have shipped
    /// a string the GUI could not draw — `✓`, `●`, `⏎` — each found by a
    /// person looking at a box where a symbol should be, because nothing else
    /// can find it: a missing glyph is not a compile error, not a panic, and
    /// not visible in any assertion on the string itself.
    ///
    /// Every row, in every language, as the GUI will actually receive it.
    #[test]
    fn every_string_the_gui_can_show_has_a_glyph_for_every_character() {
        let ctx = gui_fonts();
        let mut bad: Vec<String> = Vec::new();
        for (name, en, fr, da) in crate::i18n::Strings::table() {
            for text in [en, fr, da] {
                let missing = undrawable(&ctx, drawable(text));
                if !missing.is_empty() {
                    bad.push(format!("{name}: {missing:?} in {text:?}"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "these would render as tofu boxes. Either reword them, or add a \
             row to `SUBSTITUTIONS` mapping the character to a Phosphor icon:\n{}",
            bad.join("\n")
        );
    }

    /// The substitutions have to be an improvement, not a swap of one box for
    /// another — and the icons they map to have to actually be in the font.
    #[test]
    fn every_substitution_is_itself_drawable() {
        let ctx = gui_fonts();
        for (bad, icon) in SUBSTITUTIONS {
            assert!(
                !undrawable(&ctx, &bad.to_string()).is_empty(),
                "U+{:04X} is drawable, so substituting it only makes the text odder",
                *bad as u32
            );
            assert!(
                undrawable(&ctx, icon).is_empty(),
                "the replacement for U+{:04X} is itself a tofu box",
                *bad as u32
            );
        }
    }

    /// A string with nothing wrong with it must come back as it went in —
    /// borrowed, not rebuilt.
    #[test]
    fn an_ordinary_string_passes_straight_through() {
        let plain = "Add assert";
        assert!(std::ptr::eq(drawable(plain), plain));
    }
}
