//! Keep the terminal's *own* background in step with the theme's.
//!
//! A terminal window is rarely a whole number of character cells tall, and the
//! leftover strip below the last row is not addressable: nothing the app draws
//! can reach it, so it keeps the emulator's default background and shows as a
//! band of the user's own colour scheme under an otherwise themed screen.
//!
//! The only fix is to change what the emulator thinks its default background
//! is, which is what OSC 11 does. It is set once the theme is known, again
//! whenever the theme changes (a themed strip in *last* week's colour is the
//! same bug), and undone on the way out -- including on a panic, alongside the
//! mouse-capture and keyboard-enhancement teardown, because a shell left with
//! somebody else's background is a worse thing to leave behind than a stray
//! escape sequence.
//!
//! Terminals that don't implement OSC 11 ignore both sequences, so the cost of
//! trying is nothing and the failure mode is the strip that was already there.

use std::io::Write;

use ratatui::buffer::Buffer;
use ratatui::style::Color;

/// Ask the terminal to use `rgb` as its default background.
fn set(rgb: (u8, u8, u8)) {
    let (r, g, b) = rgb;
    // BEL-terminated rather than ST: every emulator that understands OSC 11 at
    // all accepts `\x07`, while a few older ones mishandle `\x1b\\`.
    let _ = write!(std::io::stdout(), "\x1b]11;#{r:02x}{g:02x}{b:02x}\x07");
    let _ = std::io::stdout().flush();
}

/// Put the terminal's default background back to whatever it was.
pub(crate) fn reset() {
    let _ = write!(std::io::stdout(), "\x1b]111\x07");
    let _ = std::io::stdout().flush();
}

/// The colour to hand to the terminal, for a theme background that is one.
///
/// Only a literal RGB triple is worth sending: a theme that asks for the
/// terminal's own palette (`Reset`, or an indexed colour) is by definition
/// already agreeing with the strip, and resolving an index to RGB would be
/// guessing at a palette the emulator owns.
pub(crate) fn wanted(bg: Color) -> Option<(u8, u8, u8)> {
    match bg {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// The colour the terminal should adopt, read off the frame that was drawn.
///
/// Not the theme's `bg`: that is the colour *behind* the panels, and the last
/// row of the screen is the footer, which is painted `panel`. The strip is
/// continuous with whatever is immediately above it, so the only way to be
/// right in every layout -- and to stay right if a panel ever reaches the
/// bottom edge -- is to ask the frame rather than to name a theme colour.
///
/// The most common background across the row wins, so a differently-coloured
/// key hint or a short line's padding doesn't decide it.
pub(crate) fn bottom_row_bg(buf: &Buffer) -> Color {
    let area = buf.area();
    if area.height == 0 || area.width == 0 {
        return Color::Reset;
    }
    let y = area.bottom() - 1;
    let mut best = Color::Reset;
    let mut best_count = 0usize;
    for x in area.left()..area.right() {
        let bg = buf[(x, y)].bg;
        let count = (area.left()..area.right())
            .filter(|&i| buf[(i, y)].bg == bg)
            .count();
        if count > best_count {
            best = bg;
            best_count = count;
        }
    }
    best
}

/// Send `bg` if it differs from what was last sent, and remember it.
///
/// Called once per frame: the theme can change from the theme editor, from a
/// language switch (a theme of `None` follows the language) or from restoring
/// a session, and comparing here catches all of them without every one of
/// those paths having to remember to.
pub(crate) fn sync(bg: Color, applied: &mut Option<(u8, u8, u8)>) {
    if let Some(rgb) = next(bg, *applied) {
        set(rgb);
        *applied = Some(rgb);
    }
}

/// What, if anything, the terminal needs told -- the decision on its own, so
/// it can be checked without writing escape sequences into a test's output.
fn next(bg: Color, applied: Option<(u8, u8, u8)>) -> Option<(u8, u8, u8)> {
    wanted(bg).filter(|rgb| applied != Some(*rgb))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme that names an actual colour is the only case worth sending: an
    /// indexed or default background already *is* the terminal's own.
    #[test]
    fn only_a_real_colour_is_worth_telling_the_terminal_about() {
        assert_eq!(wanted(Color::Rgb(0x1e, 0x1e, 0x2e)), Some((30, 30, 46)));
        assert_eq!(wanted(Color::Reset), None);
        assert_eq!(wanted(Color::Indexed(4)), None);
        assert_eq!(wanted(Color::Blue), None);
    }

    /// The strip joins on to the *bottom row* of the screen, which is the
    /// footer's `panel`. Matching the theme's `bg` instead left a band in a
    /// colour nothing on screen was using, because this layout covers every
    /// cell and never shows `bg` at all. A stray hint in another colour must
    /// not outvote the row it sits on.
    #[test]
    fn the_colour_matched_is_the_one_on_the_last_row() {
        use ratatui::layout::Rect;
        let panel = Color::Rgb(31, 35, 40);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        for x in 0..10 {
            buf[(x, 2)].set_bg(panel);
        }
        buf[(4, 2)].set_bg(Color::Rgb(1, 1, 1));
        buf[(5, 2)].set_bg(Color::Rgb(1, 1, 1));
        assert_eq!(bottom_row_bg(&buf), panel);
        // Rows above the last one have no bearing on the strip.
        for x in 0..10 {
            buf[(x, 0)].set_bg(Color::Rgb(9, 9, 9));
        }
        assert_eq!(bottom_row_bg(&buf), panel);
        assert_eq!(
            bottom_row_bg(&Buffer::empty(Rect::new(0, 0, 0, 0))),
            Color::Reset
        );
    }

    /// The strip has to follow a theme change, or switching themes leaves the
    /// bottom of the window in the previous one -- the same bug, one theme
    /// late. Equally, an unchanged theme must not write an escape sequence
    /// every frame.
    #[test]
    fn the_terminal_is_told_once_per_change_and_not_once_per_frame() {
        assert_eq!(next(Color::Rgb(1, 2, 3), None), Some((1, 2, 3)));
        assert_eq!(
            next(Color::Rgb(1, 2, 3), Some((1, 2, 3))),
            None,
            "an unchanged theme wrote an escape sequence anyway"
        );
        assert_eq!(
            next(Color::Rgb(9, 9, 9), Some((1, 2, 3))),
            Some((9, 9, 9)),
            "a theme change was not passed on"
        );
        // A theme that hands back the terminal's own colour leaves the last
        // one in place rather than pretending it reset it.
        assert_eq!(next(Color::Reset, Some((9, 9, 9))), None);
    }
}
