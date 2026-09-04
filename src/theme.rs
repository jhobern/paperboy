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
    /// A value the request computes for itself at send time — a `# [Gen]`
    /// row's result.
    ///
    /// Its own colour rather than reusing `ok`: green says "this has a value",
    /// which for a computed name is not yet true. The placeholder is still
    /// showing its braces, and the reason is different from every other reason
    /// braces survive (loading, failed, undefined), so it reads as a fourth
    /// thing rather than as a green value that inexplicably wasn't substituted.
    pub(crate) computed: Color,
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
    /// Hairline colour for borders and separators — the structure of the
    /// interface rather than any of its content. Separate from `dim` (which is
    /// *text*) because a line that reads correctly as a border is dimmer than
    /// the faintest text anyone is expected to read.
    pub(crate) line: Color,
}

/// Number of individually-editable colours in a theme.
pub(crate) const THEME_COLOR_COUNT: usize = 16;

/// A named, serialisable theme definition. Colours are stored as RGB triples
/// (not `ratatui::Color`) so they persist to `state.json` without depending on
/// ratatui's optional serde feature, and so the theme editor can adjust each
/// channel directly. Convert to a runtime [`Theme`] with [`ThemeSpec::to_theme`].
///
/// Deserialization is hand-written rather than derived so that a theme saved
/// before the surface colours existed still loads: the four of them are
/// optional on the way in and fall back to the shades they used to be derived
/// from, so an upgrade leaves a custom theme looking exactly as it did.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
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
    /// A value computed at send time (see [`Theme::computed`]).
    pub(crate) computed: [u8; 3],
    pub(crate) select_bg: [u8; 3],
    pub(crate) select_fg: [u8; 3],
    /// The wash behind an editable field.
    ///
    /// Its own colour rather than a shade derived from `panel`: the derivation
    /// put it two RGB points from the raised surface used for buttons and
    /// headers, so a box you can type in and a box you can press looked
    /// identical, and a dark theme became a single grey with faintly different
    /// greys drawn on it.
    pub(crate) field: [u8; 3],
    /// A raised surface: buttons, table headers, inactive tabs.
    pub(crate) raised: [u8; 3],
    /// A recessed surface: scroll bars, code blocks.
    pub(crate) sunken: [u8; 3],
    /// Borders and separators (see [`Theme::line`]).
    pub(crate) line: [u8; 3],
}

/// Blend two RGB triples — used only to age old themes forward (see the
/// `Deserialize` impl below), so their surfaces land exactly where the code
/// used to compute them.
fn blend(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    [l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2])]
}

impl<'de> Deserialize<'de> for ThemeSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            name: String,
            bg: [u8; 3],
            panel: [u8; 3],
            text: [u8; 3],
            dim: [u8; 3],
            accent: [u8; 3],
            ok: [u8; 3],
            err: [u8; 3],
            subst: [u8; 3],
            pending: [u8; 3],
            #[serde(default)]
            computed: Option<[u8; 3]>,
            select_bg: [u8; 3],
            select_fg: [u8; 3],
            #[serde(default)]
            field: Option<[u8; 3]>,
            #[serde(default)]
            raised: Option<[u8; 3]>,
            #[serde(default)]
            sunken: Option<[u8; 3]>,
            #[serde(default)]
            line: Option<[u8; 3]>,
        }
        let r = Raw::deserialize(d)?;
        Ok(ThemeSpec {
            // The exact shades the GUI used to derive, so a theme written by an
            // older version is not repainted by an upgrade.
            field: r.field.unwrap_or_else(|| blend(r.panel, r.text, 0.05)),
            raised: r.raised.unwrap_or_else(|| blend(r.panel, r.text, 0.06)),
            sunken: r.sunken.unwrap_or_else(|| blend(r.bg, [0, 0, 0], 0.10)),
            // Split the difference between the terminal UI's old border colour
            // (`dim`) and the GUI's old one (a shade of `panel`): either
            // extreme would visibly move one of the two front-ends.
            line: r.line.unwrap_or_else(|| blend(r.dim, r.panel, 0.5)),
            // A theme saved before computed values existed keeps the colour it
            // was drawn in then, which was `ok`. An upgrade repainting somebody's
            // custom theme is a worse outcome than a colour they can change.
            computed: r.computed.unwrap_or(r.ok),
            name: r.name,
            bg: r.bg,
            panel: r.panel,
            text: r.text,
            dim: r.dim,
            accent: r.accent,
            ok: r.ok,
            err: r.err,
            subst: r.subst,
            pending: r.pending,
            select_bg: r.select_bg,
            select_fg: r.select_fg,
        })
    }
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
            computed: c(self.computed),
            select_bg: c(self.select_bg),
            select_fg: c(self.select_fg),
            line: c(self.line),
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
            9 => self.computed,
            10 => self.select_bg,
            11 => self.select_fg,
            12 => self.field,
            13 => self.raised,
            14 => self.sunken,
            _ => self.line,
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
            9 => &mut self.computed,
            10 => &mut self.select_bg,
            11 => &mut self.select_fg,
            12 => &mut self.field,
            13 => &mut self.raised,
            14 => &mut self.sunken,
            _ => &mut self.line,
        };
        *slot = rgb;
    }
}

/// The default theme, and the one a fresh install starts on.
pub(crate) const PRESET_DEFAULT: &str = "Graphite";
pub(crate) const PRESET_MIDNIGHT: &str = "Midnight";
pub(crate) const PRESET_EVERGREEN: &str = "Evergreen";
pub(crate) const PRESET_ESPRESSO: &str = "Espresso";
pub(crate) const PRESET_DAYLIGHT: &str = "Daylight";
pub(crate) const PRESET_ENGLISH: &str = "Britannia";
pub(crate) const PRESET_FRENCH: &str = "Parisian Purple";
pub(crate) const PRESET_DANISH: &str = "Dannebrog";

// The default: Graphite — a near-neutral dark grey ground with a single
// restrained blue accent.
//
// Every theme now names its own surfaces (`field`, `raised`, `sunken`, `line`)
// instead of letting the GUI derive them from `panel`. The derivation put the
// field wash and the raised button wash two RGB points apart, so a screen of
// controls became one flat grey with faintly different greys on it — the
// complaint that prompted this. The ground is also darker relative to the
// panels than it was, so a panel now reads as a panel rather than as a slightly
// different patch of the same colour.
//
// The language presets below are decorative: flag colours, kept because they
// are pleasant and because "follow the language" is a setting people use. They
// are no longer garish — the school-bus yellow selection is gone — but they are
// still not what a tool should open on in an office, which is why Graphite is
// the default and why the other neutral themes sit above them in the list.
fn graphite() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_DEFAULT.to_string(),
        bg: [18, 20, 23],
        panel: [31, 35, 40],
        text: [226, 229, 233],
        dim: [140, 148, 159],
        accent: [96, 150, 200],
        ok: [92, 172, 124],
        err: [206, 99, 95],
        subst: [112, 172, 182],
        pending: [203, 157, 77],
        computed: [164, 142, 214],
        select_bg: [58, 86, 120],
        select_fg: [236, 241, 247],
        field: [40, 45, 52],
        raised: [48, 53, 61],
        sunken: [12, 13, 15],
        line: [66, 73, 84],
    }
}

// Midnight — a deep blue-black with a clear sky-blue accent. Graphite for
// people who find neutral grey cold.
fn midnight() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_MIDNIGHT.to_string(),
        bg: [13, 17, 28],
        panel: [23, 29, 45],
        text: [222, 229, 242],
        dim: [133, 145, 171],
        accent: [94, 166, 214],
        ok: [84, 177, 140],
        err: [214, 104, 104],
        subst: [122, 176, 196],
        pending: [206, 158, 86],
        computed: [158, 146, 226],
        select_bg: [48, 84, 124],
        select_fg: [234, 242, 252],
        field: [31, 39, 58],
        raised: [38, 47, 68],
        sunken: [9, 12, 20],
        line: [56, 68, 95],
    }
}

// Evergreen — a dark slate-green ground with a muted green accent; the
// quietest of the dark themes.
fn evergreen() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_EVERGREEN.to_string(),
        bg: [16, 23, 21],
        panel: [26, 37, 34],
        text: [224, 233, 228],
        dim: [136, 156, 148],
        accent: [92, 170, 132],
        ok: [110, 186, 128],
        err: [206, 104, 96],
        subst: [122, 170, 180],
        pending: [202, 162, 84],
        computed: [160, 148, 208],
        select_bg: [46, 90, 74],
        select_fg: [234, 244, 238],
        field: [34, 48, 44],
        raised: [40, 56, 51],
        sunken: [11, 16, 15],
        line: [58, 79, 72],
    }
}

// Espresso — a warm dark brown with an amber accent, for screens (and eyes)
// that dislike blue-grey at night.
fn espresso() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_ESPRESSO.to_string(),
        bg: [26, 21, 18],
        panel: [40, 33, 28],
        text: [236, 228, 218],
        dim: [160, 146, 132],
        accent: [206, 146, 74],
        ok: [140, 175, 106],
        err: [212, 102, 86],
        subst: [150, 176, 170],
        pending: [216, 170, 90],
        computed: [188, 156, 216],
        select_bg: [96, 72, 46],
        select_fg: [250, 242, 230],
        field: [49, 40, 34],
        raised: [58, 48, 40],
        sunken: [18, 14, 12],
        line: [80, 66, 55],
    }
}

// Daylight — the one light theme: a soft grey window with white panels, a
// recessed field wash and a clearly darker raised surface for controls. Every
// status colour is darkened rather than reused, since the dark themes' colours
// are chosen for contrast against near-black and would be illegible here.
fn daylight() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_DAYLIGHT.to_string(),
        bg: [240, 242, 246],
        panel: [255, 255, 255],
        text: [28, 32, 38],
        dim: [96, 105, 118],
        accent: [28, 104, 178],
        ok: [24, 122, 76],
        err: [186, 48, 44],
        subst: [18, 114, 124],
        pending: [160, 104, 16],
        computed: [110, 70, 176],
        select_bg: [196, 220, 246],
        select_fg: [16, 20, 26],
        field: [238, 241, 246],
        raised: [226, 231, 239],
        sunken: [214, 219, 227],
        line: [199, 206, 216],
    }
}

// English: Britannia — Union navy with a claret accent and parchment text.
fn britannia() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_ENGLISH.to_string(),
        bg: [15, 22, 46],
        panel: [24, 34, 66],
        text: [238, 235, 227],
        dim: [150, 164, 194],
        accent: [186, 66, 82],
        ok: [84, 178, 132],
        err: [226, 92, 88],
        subst: [118, 182, 196],
        pending: [214, 162, 72],
        computed: [156, 142, 216],
        select_bg: [146, 116, 32],
        select_fg: [252, 248, 236],
        field: [31, 43, 79],
        raised: [38, 52, 92],
        sunken: [11, 16, 34],
        line: [56, 74, 120],
    }
}

// French: Parisian Purple — warm plum with an amethyst-lavender accent.
fn parisian_purple() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_FRENCH.to_string(),
        bg: [28, 23, 42],
        panel: [42, 35, 62],
        text: [238, 232, 226],
        dim: [166, 154, 186],
        accent: [174, 138, 220],
        ok: [96, 180, 140],
        err: [220, 100, 96],
        subst: [126, 178, 192],
        pending: [212, 164, 80],
        computed: [210, 146, 198],
        select_bg: [96, 72, 140],
        select_fg: [244, 238, 250],
        field: [51, 42, 74],
        raised: [60, 50, 86],
        sunken: [20, 16, 30],
        line: [78, 66, 110],
    }
}

// Danish: Dannebrog — deep red ground with a soft crimson accent, echoing the
// Danish flag.
fn dannebrog() -> ThemeSpec {
    ThemeSpec {
        name: PRESET_DANISH.to_string(),
        bg: [28, 19, 21],
        panel: [44, 31, 33],
        text: [240, 233, 228],
        dim: [180, 160, 158],
        accent: [214, 66, 84],
        ok: [110, 180, 128],
        err: [226, 96, 92],
        subst: [134, 178, 186],
        pending: [214, 164, 80],
        computed: [162, 144, 216],
        select_bg: [150, 116, 52],
        select_fg: [252, 246, 236],
        field: [54, 38, 40],
        raised: [64, 45, 47],
        sunken: [19, 13, 14],
        line: [86, 60, 63],
    }
}

/// The localised label for the `i`th theme colour, matching [`ThemeSpec::color`]
/// order.
///
/// Shared by both theme editors: the two front-ends had a copy each, which is
/// exactly the kind of table that drifts the moment a colour is added.
pub(crate) fn color_label(s: &crate::i18n::Strings, i: usize) -> &'static str {
    match i {
        0 => s.theme_c_bg,
        1 => s.theme_c_panel,
        2 => s.theme_c_text,
        3 => s.theme_c_dim,
        4 => s.theme_c_accent,
        5 => s.theme_c_ok,
        6 => s.theme_c_err,
        7 => s.theme_c_subst,
        8 => s.theme_c_pending,
        9 => s.theme_c_computed,
        10 => s.theme_c_select_bg,
        11 => s.theme_c_select_fg,
        12 => s.theme_c_field,
        13 => s.theme_c_raised,
        14 => s.theme_c_sunken,
        _ => s.theme_c_line,
    }
}

/// The built-in presets, in display order: the default first, then one per
/// bundled language.
pub(crate) fn builtin_presets() -> Vec<ThemeSpec> {
    vec![
        graphite(),
        midnight(),
        evergreen(),
        espresso(),
        daylight(),
        britannia(),
        parisian_purple(),
        dannebrog(),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance, so "are these two surfaces actually different?" can
    /// be asked as a number rather than by squinting at a screenshot.
    fn luminance([r, g, b]: [u8; 3]) -> f32 {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
        let (hi, lo) = {
            let (x, y) = (luminance(a), luminance(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The complaint that produced these colours: the background and every box
    /// drawn on it were the same dark grey. The surfaces were derived from
    /// `panel` with such small factors that a text field and a button landed
    /// two RGB points apart, so a panel of controls read as one flat sheet.
    /// Each preset must keep its surfaces apart — including, especially, the
    /// field wash and the raised surface.
    #[test]
    fn every_preset_keeps_its_surfaces_visibly_apart() {
        for p in builtin_presets() {
            let name = &p.name;
            assert!(
                contrast(p.panel, p.bg) >= 1.08,
                "{name}: a panel must read as a panel against the ground"
            );
            assert!(
                contrast(p.field, p.raised) >= 1.05,
                "{name}: a box you type in must not look like a box you press"
            );
            assert!(
                contrast(p.line, p.panel) >= 1.4,
                "{name}: a border has to be visible on the panel it edges"
            );
            assert!(
                contrast(p.text, p.field) >= 7.0,
                "{name}: text in a field stays comfortably readable"
            );
            assert!(
                contrast(p.select_fg, p.select_bg) >= 3.5,
                "{name}: a selected row stays readable"
            );
        }
    }

    /// A theme saved before the surface colours existed must still load — and
    /// must look exactly as it did, rather than being quietly repainted by an
    /// upgrade.
    #[test]
    fn a_theme_saved_without_the_surface_colours_still_loads_unchanged() {
        let old = r#"{
            "name": "Sunset",
            "bg": [20, 20, 30], "panel": [40, 40, 60], "text": [230, 230, 235],
            "dim": [140, 140, 150], "accent": [200, 120, 60], "ok": [90, 170, 120],
            "err": [200, 90, 90], "subst": [110, 170, 180], "pending": [200, 150, 70],
            "select_bg": [60, 80, 110], "select_fg": [235, 240, 246]
        }"#;
        let spec: ThemeSpec = serde_json::from_str(old).expect("an old theme still loads");
        assert_eq!(spec.name, "Sunset");
        // The exact shades the GUI used to mix out of `panel`/`bg`.
        assert_eq!(spec.field, blend([40, 40, 60], [230, 230, 235], 0.05));
        assert_eq!(spec.raised, blend([40, 40, 60], [230, 230, 235], 0.06));
        assert_eq!(spec.sunken, blend([20, 20, 30], [0, 0, 0], 0.10));
        assert_eq!(spec.line, blend([140, 140, 150], [40, 40, 60], 0.5));
        // Computed values were drawn in `ok` before they had a colour of their
        // own, so a theme written back then keeps looking exactly as it did.
        assert_eq!(spec.computed, [90, 170, 120]);
    }

    /// Every colour is reachable and settable by index, or the theme editors
    /// silently stop at whatever the count used to be.
    #[test]
    fn every_theme_colour_round_trips_through_its_index() {
        let mut spec = default_preset();
        for i in 0..THEME_COLOR_COUNT {
            spec.set_color(i, [i as u8, 7, 9]);
        }
        for i in 0..THEME_COLOR_COUNT {
            assert_eq!(spec.color(i), [i as u8, 7, 9], "colour {i} round-trips");
        }
    }
}
