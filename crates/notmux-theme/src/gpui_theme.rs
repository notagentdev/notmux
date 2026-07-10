//! Theme adapter that maps `notagent_theme` color definitions to GPUI `Hsla` values.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::Hsla;
use notagent_theme::{Theme, ThemeBg, ThemeColor};

/// Converts an RGB tuple (0-255) to a GPUI `Hsla` color.
fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let lightness = (max + min) / 2.0;

    let saturation = if delta == 0.0 {
        0.0
    } else if lightness <= 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };

    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let hue = if hue < 0.0 { hue + 360.0 } else { hue };
    let hue = hue / 360.0;

    Hsla { h: hue, s: saturation, l: lightness, a: 1.0 }
}

/// Parses a hex color string (e.g. `#1e222a`) into an RGB tuple.
#[allow(dead_code)]
fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let cleaned = hex.trim_start_matches('#');
    if cleaned.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&cleaned[0..2], 16).ok()?;
    let g = u8::from_str_radix(&cleaned[2..4], 16).ok()?;
    let b = u8::from_str_radix(&cleaned[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Converts an ANSI-256 color index to an RGB tuple using the standard xterm palette.
fn ansi_256_to_rgb(index: u8) -> (u8, u8, u8) {
    const BASIC_COLORS: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];

    if index < 16 {
        return BASIC_COLORS[index as usize];
    }
    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_val = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        return (to_val(r), to_val(g), to_val(b));
    }
    let gray = 8 + (index - 232) * 10;
    (gray, gray, gray)
}

/// Extracts the raw color value from a theme's ANSI escape string.
///
/// Theme stores colors as ANSI escape strings like `\x1b[38;2;138;139;190m`
/// (truecolor) or `\x1b[38;5;103m` (256-color). This function parses those
/// strings back into RGB tuples.
fn ansi_to_rgb(ansi: &str) -> Option<(u8, u8, u8)> {
    let ansi = ansi.trim();

    // Truecolor: \x1b[38;2;R;G;Bm (fg) or \x1b[48;2;R;G;Bm (bg)
    if ansi.contains(";2;") {
        let parts: Vec<&str> = ansi
            .trim_start_matches("\x1b[")
            .trim_end_matches('m')
            .split(';')
            .collect();
        if parts.len() >= 5 {
            let r = parts[2].parse::<u8>().ok()?;
            let g = parts[3].parse::<u8>().ok()?;
            let b = parts[4].parse::<u8>().ok()?;
            return Some((r, g, b));
        }
    }

    // 256-color: \x1b[38;5;Nm (fg) or \x1b[48;5;Nm (bg)
    if ansi.contains(";5;") {
        let parts: Vec<&str> = ansi
            .trim_start_matches("\x1b[")
            .trim_end_matches('m')
            .split(';')
            .collect();
        if parts.len() >= 3 {
            let n = parts[2].parse::<u8>().ok()?;
            return Some(ansi_256_to_rgb(n));
        }
    }

    None
}

/// A GPUI-compatible theme that caches `Hsla` values for all theme colors.
///
/// Wraps a `notagent_theme::Theme` and resolves its ANSI-based color definitions
/// to GPUI `Hsla` values at construction time.
#[derive(Clone)]
pub struct GpuiTheme {
    theme: Arc<Theme>,
    fg_colors: HashMap<ThemeColor, Hsla>,
    bg_colors: HashMap<ThemeBg, Hsla>,
    /// Page/card/info background surfaces resolved from the theme's export colors
    /// (so the whole UI re-backgrounds with the selected theme).
    page_bg: Hsla,
    card_bg: Hsla,
    info_bg: Hsla,
}
impl gpui::Global for GpuiTheme {}

impl GpuiTheme {
    /// Creates a new `GpuiTheme` from a `notagent_theme::Theme`.
    #[must_use]
    pub fn new(theme: Arc<Theme>) -> Self {
        let fg_variants = [
            ThemeColor::Accent,
            ThemeColor::Border,
            ThemeColor::BorderAccent,
            ThemeColor::BorderMuted,
            ThemeColor::Success,
            ThemeColor::Error,
            ThemeColor::Warning,
            ThemeColor::Muted,
            ThemeColor::Dim,
            ThemeColor::Text,
            ThemeColor::ThinkingText,
            ThemeColor::UserMessageText,
            ThemeColor::CustomMessageText,
            ThemeColor::CustomMessageLabel,
            ThemeColor::ToolTitle,
            ThemeColor::ToolOutput,
            ThemeColor::MdHeading,
            ThemeColor::MdLink,
            ThemeColor::MdLinkUrl,
            ThemeColor::MdCode,
            ThemeColor::MdCodeBlock,
            ThemeColor::MdCodeBlockBorder,
            ThemeColor::MdQuote,
            ThemeColor::MdQuoteBorder,
            ThemeColor::MdHr,
            ThemeColor::MdListBullet,
            ThemeColor::ToolDiffAdded,
            ThemeColor::ToolDiffRemoved,
            ThemeColor::ToolDiffContext,
            ThemeColor::SyntaxComment,
            ThemeColor::SyntaxKeyword,
            ThemeColor::SyntaxFunction,
            ThemeColor::SyntaxVariable,
            ThemeColor::SyntaxString,
            ThemeColor::SyntaxNumber,
            ThemeColor::SyntaxType,
            ThemeColor::SyntaxOperator,
            ThemeColor::SyntaxPunctuation,
            ThemeColor::ThinkingOff,
            ThemeColor::ThinkingMinimal,
            ThemeColor::ThinkingLow,
            ThemeColor::ThinkingMedium,
            ThemeColor::ThinkingHigh,
            ThemeColor::ThinkingXhigh,
            ThemeColor::BashMode,
            ThemeColor::ModePlan,
            ThemeColor::ModeAcceptEdits,
            ThemeColor::ModeAuto,
            ThemeColor::ModeYolo,
        ];
        let mut fg_colors = HashMap::new();
        for color in &fg_variants {
            let ansi = theme.get_fg_ansi(*color);
            if let Some(rgb) = ansi_to_rgb(&ansi) {
                fg_colors.insert(*color, rgb_to_hsla(rgb.0, rgb.1, rgb.2));
            }
        }
        let bg_variants = [
            ThemeBg::SelectedBg,
            ThemeBg::UserMessageBg,
            ThemeBg::CustomMessageBg,
            ThemeBg::ToolPendingBg,
            ThemeBg::ToolSuccessBg,
            ThemeBg::ToolErrorBg,
        ];
        let mut bg_colors = HashMap::new();
        for bg in &bg_variants {
            let ansi = theme.get_bg_ansi(*bg);
            if let Some(rgb) = ansi_to_rgb(&ansi) {
                bg_colors.insert(*bg, rgb_to_hsla(rgb.0, rgb.1, rgb.2));
            }
        }
        // Background surfaces from the theme's export colors (with the previous
        // hardcoded values as fallbacks if a theme omits them).
        let export = notagent_theme::get_theme_export_colors(theme.name().as_deref());
        let resolve = |hex: &Option<String>| {
            hex.as_deref()
                .and_then(parse_hex)
                .map(|(r, g, b)| rgb_to_hsla(r, g, b))
        };
        let page_bg = resolve(&export.page_bg).unwrap_or_else(|| rgb_to_hsla(18, 18, 24));
        let card_bg = resolve(&export.card_bg).unwrap_or_else(|| rgb_to_hsla(30, 30, 40));
        let info_bg = resolve(&export.info_bg).unwrap_or_else(|| rgb_to_hsla(45, 45, 55));

        Self { theme, fg_colors, bg_colors, page_bg, card_bg, info_bg }
    }

    /// Returns the `Hsla` for a foreground `ThemeColor`, falling back to white.
    #[must_use]
    pub fn fg(&self, color: ThemeColor) -> Hsla {
        self.fg_colors.get(&color).copied().unwrap_or(gpui::white())
    }

    /// Returns the `Hsla` for a background `ThemeBg`, falling back to transparent.
    #[must_use]
    pub fn bg(&self, bg: ThemeBg) -> Hsla {
        self.bg_colors
            .get(&bg)
            .copied()
            .unwrap_or(gpui::transparent_black())
    }
    #[must_use]
    pub fn surface(&self) -> Hsla {
        self.card_bg
    }
    #[must_use]
    pub fn surface_dim(&self) -> Hsla {
        self.page_bg
    }
    #[must_use]
    pub fn surface_bright(&self) -> Hsla {
        self.info_bg
    }

    /// The base (page) background color.
    #[must_use]
    pub fn bg_base(&self) -> Hsla {
        self.surface_dim()
    }

    /// Elevated card/block surface: a barely-lighter translucent layer over the
    /// page background. Levels 1–3 increase in subtle elevation.
    #[must_use]
    pub fn surface_1(&self) -> Hsla {
        blend(self.bg_base(), self.fg(ThemeColor::Text), 0.05)
    }
    #[must_use]
    pub fn surface_2(&self) -> Hsla {
        blend(self.bg_base(), self.fg(ThemeColor::Text), 0.09)
    }
    #[must_use]
    pub fn surface_3(&self) -> Hsla {
        blend(self.bg_base(), self.fg(ThemeColor::Text), 0.14)
    }

    /// Subtle 1px border/outline color used to delineate surfaces.
    #[must_use]
    pub fn outline(&self) -> Hsla {
        blend(self.bg_base(), self.fg(ThemeColor::Text), 0.12)
    }

    /// A clearly visible translucent highlight for selected text.
    #[must_use]
    pub fn selection(&self) -> Hsla {
        let mut color = self.fg(ThemeColor::Accent);
        color.a = 0.4;
        color
    }

    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }
}

/// Alpha-composites `over` onto `base` and returns an opaque color.
fn blend(base: Hsla, over: Hsla, alpha: f32) -> Hsla {
    let base = gpui::Rgba::from(base);
    let over = gpui::Rgba::from(over);
    let mix = |a: f32, b: f32| a * (1.0 - alpha) + b * alpha;
    gpui::Rgba {
        r: mix(base.r, over.r),
        g: mix(base.g, over.g),
        b: mix(base.b, over.b),
        a: 1.0,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::{GpuiTheme, Hsla, ansi_256_to_rgb, ansi_to_rgb, parse_hex, rgb_to_hsla};
    use notagent_theme::{Theme, ThemeColor};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[test]
    fn test_rgb_to_hsla_black() {
        let actual = rgb_to_hsla(0, 0, 0);
        let expected = Hsla { h: 0.0, s: 0.0, l: 0.0, a: 1.0 };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_rgb_to_hsla_white() {
        let actual = rgb_to_hsla(255, 255, 255);
        let expected = Hsla { h: 0.0, s: 0.0, l: 1.0, a: 1.0 };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_parse_hex_valid() {
        let actual = parse_hex("#1e222a");
        let expected = Some((30, 34, 42));
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_parse_hex_without_hash() {
        let actual = parse_hex("ff0000");
        let expected = Some((255, 0, 0));
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_parse_hex_invalid_length() {
        let actual = parse_hex("#fff");
        let expected = None;
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_ansi_to_rgb_truecolor() {
        let actual = ansi_to_rgb("\x1b[38;2;138;139;190m");
        let expected = Some((138, 139, 190));
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_ansi_to_rgb_256_color() {
        let actual = ansi_to_rgb("\x1b[38;5;103m");
        let expected = Some(ansi_256_to_rgb(103));
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_ansi_to_rgb_empty() {
        let actual = ansi_to_rgb("");
        let expected = None;
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_gpui_theme_from_builtin_dark() {
        let theme = Arc::new(Theme::load_default("dark").expect("dark theme should load"));
        let gpui_theme = GpuiTheme::new(theme);
        let accent = gpui_theme.fg(ThemeColor::Accent);
        assert_ne!(
            accent,
            gpui::white(),
            "accent should differ from fallback white"
        );
    }
}
