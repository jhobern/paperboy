use ratatui::style::Color;

use crate::i18n::Language;

pub(crate) struct Theme {
    pub(crate) bg: Color,
    pub(crate) panel: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) accent: Color,
    pub(crate) ok: Color,
    pub(crate) err: Color,
    /// Colour for values substituted from the environment into the request
    /// preview — signals that the shown text is a representation, not the source.
    pub(crate) subst: Color,
    /// Colour for a substitution that is still loading (pending secret): orange.
    pub(crate) pending: Color,
    /// Background for the app's own Request JSON / Response text selection
    /// highlight. Deliberately a flat, explicit colour rather than
    /// `Modifier::REVERSED` (simple fg/bg inversion): most terminals render
    /// their own native (Shift-drag) selection as plain reverse video too,
    /// so a bare inversion here would look identical to — and be easily
    /// confused with — a selection the app never even sees. A fixed colour
    /// instead reads as unambiguously the app's own highlight in any
    /// terminal color scheme.
    pub(crate) select_bg: Color,
    /// Foreground used on top of `select_bg`, chosen for contrast against it.
    pub(crate) select_fg: Color,
}

pub(crate) fn theme(lang: &Language) -> Theme {
    match lang {
        // French: Provence — warm plum/aubergine, soft amethyst-lavender accent,
        // cream text.
        Language::French => Theme {
            bg: Color::Rgb(34, 28, 50),
            panel: Color::Rgb(48, 40, 70),
            text: Color::Rgb(238, 232, 226),
            dim: Color::Rgb(170, 158, 190),
            accent: Color::Rgb(180, 142, 226),
            ok: Color::Rgb(73, 204, 144),
            err: Color::Rgb(248, 81, 73),
            subst: Color::Rgb(120, 200, 210),
            pending: Color::Rgb(252, 161, 48),
            select_bg: Color::Rgb(255, 214, 10),
            select_fg: Color::Rgb(20, 20, 20),
        },
        Language::Danish => Theme {
            bg: Color::Rgb(30, 22, 24),
            panel: Color::Rgb(44, 33, 35),
            text: Color::Rgb(240, 233, 228),
            dim: Color::Rgb(184, 166, 164),
            accent: Color::Rgb(224, 62, 84),
            ok: Color::Rgb(73, 204, 144),
            err: Color::Rgb(248, 81, 73),
            subst: Color::Rgb(120, 200, 210),
            pending: Color::Rgb(252, 161, 48),
            select_bg: Color::Rgb(255, 214, 10),
            select_fg: Color::Rgb(20, 20, 20),
        },
        // English (default): Britannia — royal Union navy, soft claret-rose
        // accent, parchment cream text.
        _ => Theme {
            bg: Color::Rgb(18, 26, 54),
            panel: Color::Rgb(26, 36, 70),
            text: Color::Rgb(240, 236, 226),
            dim: Color::Rgb(150, 164, 194),
            accent: Color::Rgb(178, 58, 74),
            ok: Color::Rgb(73, 204, 144),
            err: Color::Rgb(248, 81, 73),
            subst: Color::Rgb(120, 200, 210),
            pending: Color::Rgb(252, 161, 48),
            select_bg: Color::Rgb(255, 214, 10),
            select_fg: Color::Rgb(20, 20, 20),
        },
    }
}

pub(crate) fn method_color(method: &str) -> Color {
    match crate::hurl::method_rgb(method) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => Color::Gray,
    }
}
