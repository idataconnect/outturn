//! The few looks a document can take.
//!
//! Named rather than described: a model choosing between three looks chooses
//! well, and one describing a look in detail writes the same headings and
//! tables whatever it is given. Every model gets the same good output because
//! the design lives here, not in the prompt.

pub(crate) type Color = (u8, u8, u8);

pub(crate) const INK: Color = (0x22, 0x22, 0x22);
const NAVY: Color = (0x1f, 0x3a, 0x5f);
const GREY: Color = (0xf3, 0xf4, 0xf6);
const BLUE: Color = (0x1a, 0x55, 0xb0);

/// The themes, by the names a caller gives them, the default first.
pub const THEMES: [&str; 3] = ["report", "letter", "plain"];

/// How one document looks, beyond what every look shares.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Rules, quote bars and table header rules; headings too where
    /// `colored_headings` is set.
    pub(crate) accent: Color,
    /// The ground of a table's header row, where it has one.
    pub(crate) header_fill: Option<Color>,
    pub(crate) colored_headings: bool,
    /// Links. Still clickable in every theme; colored only where the page
    /// will be read on a screen.
    pub(crate) link: Color,
    /// The size an opening heading is set at, and whether a rule runs under it.
    pub(crate) title: f32,
    pub(crate) title_rule: bool,
    /// Whether pages after the first say what document they are from.
    pub(crate) running_header: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self::named(None, None).expect("the default theme")
    }
}

impl Theme {
    /// A theme by name, in an accent color where one is given as `#rrggbb`.
    /// Errors say what would have been accepted.
    pub fn named(name: Option<&str>, accent: Option<&str>) -> Result<Theme, String> {
        let mut theme = match name.unwrap_or(THEMES[0]) {
            // Colored, titled and headed: a document that reads as designed.
            "report" => Theme {
                accent: NAVY,
                header_fill: Some(tint(NAVY)),
                colored_headings: true,
                link: BLUE,
                title: 24.0,
                title_rule: true,
                running_header: true,
            },
            // A letter is read once, from the top: a modest title, ink
            // headings, and nothing repeated at the top of its pages.
            "letter" => Theme {
                accent: NAVY,
                header_fill: None,
                colored_headings: false,
                link: BLUE,
                title: 16.0,
                title_rule: false,
                running_header: false,
            },
            // Black and grey, for a document that will be printed or faxed
            // on whatever is to hand.
            "plain" => Theme {
                accent: INK,
                header_fill: Some(GREY),
                colored_headings: false,
                link: INK,
                title: 22.0,
                title_rule: true,
                running_header: true,
            },
            other => {
                return Err(format!(
                    "there is no theme called {other:?}; the themes are {}",
                    THEMES.join(", ")
                ));
            }
        };
        if let Some(accent) = accent {
            let color = parse_color(accent)?;
            theme.accent = color;
            if theme.header_fill.is_some() {
                theme.header_fill = Some(tint(color));
            }
        }
        Ok(theme)
    }
}

/// A `#rrggbb` color dark enough to read as text and rules on white.
fn parse_color(text: &str) -> Result<Color, String> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let (Some(r), Some(g), Some(b), 6) = (channel(0), channel(2), channel(4), hex.len()) else {
        return Err(format!(
            "the accent {text:?} is not a color; give one as #rrggbb, such as #1f3a5f"
        ));
    };
    // Relative luminance, roughly: past this, headings in it on white are
    // hard to read, and a pale accent is not what anyone asked for in a
    // document meant to be read.
    let light = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    if light > 150.0 {
        return Err(format!(
            "the accent {text} is too light to read on white; choose a darker one"
        ));
    }
    Ok((r, g, b))
}

/// A color thinned toward white, for a ground under text.
fn tint((r, g, b): Color) -> Color {
    let thin = |c: u8| (f32::from(c) * 0.12 + 255.0 * 0.88).round() as u8;
    (thin(r), thin(g), thin(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_theme_exists_and_the_default_is_the_first() {
        for name in THEMES {
            Theme::named(Some(name), None).unwrap();
        }
        assert_eq!(
            Theme::default(),
            Theme::named(Some(THEMES[0]), None).unwrap()
        );
    }

    #[test]
    fn an_unknown_theme_lists_the_real_ones() {
        let err = Theme::named(Some("fancy"), None).unwrap_err();
        assert!(err.contains("report, letter, plain"), "{err}");
    }

    #[test]
    fn an_accent_colors_the_theme_and_its_tint() {
        let theme = Theme::named(Some("report"), Some("#7a1f2b")).unwrap();
        assert_eq!(theme.accent, (0x7a, 0x1f, 0x2b));
        assert_eq!(theme.header_fill, Some(tint((0x7a, 0x1f, 0x2b))));
        let letter = Theme::named(Some("letter"), Some("7a1f2b")).unwrap();
        assert_eq!(letter.header_fill, None);
    }

    #[test]
    fn an_accent_that_is_not_a_color_or_is_too_pale_is_refused() {
        for bad in ["red", "#12345", "#gggggg", "#1234567"] {
            assert!(Theme::named(None, Some(bad)).is_err(), "{bad}");
        }
        let err = Theme::named(None, Some("#ffee88")).unwrap_err();
        assert!(err.contains("too light"), "{err}");
    }
}
