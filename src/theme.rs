use ratatui::style::Color;
use serde::{Deserialize, Serialize};

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

/// Number of individually-editable colours in a theme.
pub(crate) const THEME_COLOR_COUNT: usize = 11;

/// A named, serialisable theme definition. Colours are stored as RGB triples
/// (not `ratatui::Color`) so they persist to `state.json` without depending on
/// ratatui's optional serde feature, and so the theme editor can adjust each
/// channel directly. Convert to a runtime [`Theme`] with [`ThemeSpec::to_theme`].
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ThemeSpec {
    pub(crate) name: String,
    pub(crate) bg: [u8; 3],
    pub(crate) panel: [u8; 3],
    pub(crate) text: [u8; 3],
    pub(crate) dim: [u8; 3],
    pub(crate) accent: [u8; 3],
    pub(crate) ok: [u8; 3],
    pub(crate) err: [u8; 3],
    pub(crate) subst: [u8; 3],
    pub(crate) pending: [u8; 3],
    pub(crate) select_bg: [u8; 3],
    pub(crate) select_fg: [u8; 3],
}

impl ThemeSpec {
    pub(crate) fn to_theme(&self) -> Theme {
        let c = |[r, g, b]: [u8; 3]| Color::Rgb(r, g, b);
        Theme {
            bg: c(self.bg),
            panel: c(self.panel),
            text: c(self.text),
            dim: c(self.dim),
            accent: c(self.accent),
            ok: c(self.ok),
            err: c(self.err),
            subst: c(self.subst),
            pending: c(self.pending),
            select_bg: c(self.select_bg),
            select_fg: c(self.select_fg),
        }
    }

    /// The `i`th editable colour (`0..THEME_COLOR_COUNT`), in display order.
    pub(crate) fn color(&self, i: usize) -> [u8; 3] {
        match i {
            0 => self.bg,
            1 => self.panel,
            2 => self.text,
            3 => self.dim,
            4 => self.accent,
            5 => self.ok,
            6 => self.err,
            7 => self.subst,
            8 => self.pending,
            9 => self.select_bg,
            _ => self.select_fg,
        }
    }

    pub(crate) fn set_color(&mut self, i: usize, rgb: [u8; 3]) {
        let slot = match i {
            0 => &mut self.bg,
            1 => &mut self.panel,
            2 => &mut self.text,
            3 => &mut self.dim,
            4 => &mut self.accent,
            5 => &mut self.ok,
            6 => &mut self.err,
            7 => &mut self.subst,
            8 => &mut self.pending,
            9 => &mut self.select_bg,
            _ => &mut self.select_fg,
        };
        *slot = rgb;
    }
}

/// The default theme, and the one a fresh install starts on.
pub(crate) const PRESET_DEFAULT: &str = "Graphite";
pub(crate) const PRESET_ENGLISH: &str = "Britannia";
pub(crate) const PRESET_FRENCH: &str = "Parisian Purple";
pub(crate) const PRESET_DANISH: &str = "Dannebrog";

// The default: Graphite — a near-neutral dark grey ground with a single
// restrained blue accent.
//
// The language presets below are decorative: saturated flag colours with a
// bright yellow selection. They are pleasant, and they stay, but they are not
// what a tool should open on in an office — professional users read heavily
// coloured chrome as unserious, and the report editor's blocks (which tint
// themselves from `accent`, `ok`, `subst` and `pending`) amplify whatever the
// theme gives them. Graphite keeps every hue distinguishable while spending far
// less of the screen on colour: the status colours are pulled toward grey, the
// selection is a quiet slate blue rather than school-bus yellow, and the accent
// is the only strong hue in the interface.
fn graphite() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_DEFAULT.to_string(),
        bg: [22, 24, 27],
        panel: [30, 33, 37],
        text: [226, 229, 233],
        dim: [138, 145, 155],
        accent: [88, 142, 191],
        ok: [92, 168, 122],
        err: [201, 96, 92],
        subst: [110, 168, 178],
        pending: [200, 154, 74],
        select_bg: [58, 82, 112],
        select_fg: [235, 240, 246],
    }
}

// English: Britannia — royal Union navy, soft claret-rose accent,
// parchment cream text (evoking the Union Jack).
fn britannia() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_ENGLISH.to_string(),
        bg: [18, 26, 54],
        panel: [26, 36, 70],
        text: [240, 236, 226],
        dim: [150, 164, 194],
        accent: [178, 58, 74],
        ok: [73, 204, 144],
        err: [248, 81, 73],
        subst: [120, 200, 210],
        pending: [252, 161, 48],
        select_bg: [255, 214, 10],
        select_fg: [20, 20, 20],
    }
}

// French: Parisian Purple — warm plum/aubergine, soft amethyst-lavender accent,
// cream text.
fn parisian_purple() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_FRENCH.to_string(),
        bg: [34, 28, 50],
        panel: [48, 40, 70],
        text: [238, 232, 226],
        dim: [170, 158, 190],
        accent: [180, 142, 226],
        ok: [73, 204, 144],
        err: [248, 81, 73],
        subst: [120, 200, 210],
        pending: [252, 161, 48],
        select_bg: [255, 214, 10],
        select_fg: [20, 20, 20],
    }
}

// Danish: Dannebrog — deep red ground with a soft crimson accent, echoing the
// Danish flag.
fn dannebrog() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_DANISH.to_string(),
        bg: [30, 22, 24],
        panel: [44, 33, 35],
        text: [240, 233, 228],
        dim: [184, 166, 164],
        accent: [224, 62, 84],
        ok: [73, 204, 144],
        err: [248, 81, 73],
        subst: [120, 200, 210],
        pending: [252, 161, 48],
        select_bg: [255, 214, 10],
        select_fg: [20, 20, 20],
    }
}

/// The built-in presets, in display order: the default first, then one per
/// bundled language.
pub(crate) fn builtin_presets() -> Vec<ThemeSpec> {
    vec![graphite(), britannia(), parisian_purple(), dannebrog()]
}

/// The theme a fresh install starts on (see [`crate::session::Session::default`]).
///
/// Deliberately not the same idea as [`preset_for_language`]: "Follow language"
/// remains a thing the user can ask for, it just isn't what they get by
/// default. An existing install keeps whatever it had — a theme is a setting,
/// and an upgrade that silently repaints someone's editor is its own kind of
/// unprofessional.
pub(crate) fn default_preset() -> ThemeSpec {
    graphite()
}

/// The preset a language defaults to when the user hasn't chosen a theme.
pub(crate) fn preset_for_language(lang: &Language) -> ThemeSpec {
    match lang {
        Language::French => parisian_purple(),
        Language::Danish => dannebrog(),
        Language::English => britannia(),
    }
}

/// Whether `name` is a built-in preset (presets can't be edited or deleted).
pub(crate) fn is_builtin(name: &str) -> bool {
    builtin_presets().iter().any(|p| p.name == name)
}

/// The runtime theme for a language's preset. Retained for callers/tests that
/// want a language preset directly; the live app resolves through
/// [`crate::tui::app::TuiApp::theme`] to honour custom themes.
#[cfg(test)]
pub(crate) fn theme(lang: &Language) -> Theme {
    preset_for_language(lang).to_theme()
}

pub(crate) fn method_color(method: &str) -> Color {
    match crate::hurl::method_rgb(method) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => Color::Gray,
    }
}
