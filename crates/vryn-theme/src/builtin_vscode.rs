//! Built-in VS Code themes bundled into the binary.
//!
//! These are shipped alongside the native Vryn themes so that new users
//! have a curated set of themes available without manual installation.
//! They share the `custom:builtin-*` ID prefix so the theme picker and
//! persistence code treats them like any other VS Code theme.

use std::sync::OnceLock;

use vryn_core::theme::{DARK_THEME, LIGHT_THEME, ThemeColors, ThemeInfo};

use crate::vscode;

struct BuiltinThemeSpec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    json: &'static str,
}

const SPECS: &[BuiltinThemeSpec] = &[
    BuiltinThemeSpec {
        id: "builtin-poimandres-dark",
        name: "Poimandres Dark",
        description: "Deep-space dark theme (VS Code community)",
        json: include_str!("../assets/themes/poimandres-dark.json"),
    },
    BuiltinThemeSpec {
        id: "builtin-poimandres-light",
        name: "Poimandres Light",
        description: "Soft light counterpart of Poimandres",
        json: include_str!("../assets/themes/poimandres-light.json"),
    },
    BuiltinThemeSpec {
        id: "builtin-dracula",
        name: "Dracula",
        description: "Classic Dracula palette",
        json: include_str!("../assets/themes/dracula.json"),
    },
    BuiltinThemeSpec {
        id: "builtin-onedark-pro",
        name: "One Dark Pro",
        description: "Atom's iconic One Dark, polished for VS Code (Binaryify)",
        json: include_str!("../assets/themes/onedark-pro.json"),
    },
    BuiltinThemeSpec {
        id: "builtin-tokyo-night",
        name: "Tokyo Night",
        description: "Clean dark theme inspired by Tokyo at night",
        json: include_str!("../assets/themes/tokyo-night.json"),
    },
    BuiltinThemeSpec {
        id: "builtin-alucard",
        name: "Alucard",
        description: "Light counterpart of Dracula (dracula/cursor)",
        json: include_str!("../assets/themes/alucard.json"),
    },
];

/// The default theme id used on first launch (no prior settings).
pub const DEFAULT_THEME_ID: &str = "builtin-poimandres-dark";

/// Load and cache all bundled VS Code themes.
pub fn builtin_themes() -> &'static [(ThemeInfo, ThemeColors)] {
    static CACHE: OnceLock<Vec<(ThemeInfo, ThemeColors)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        SPECS
            .iter()
            .filter_map(|spec| {
                let stripped = vscode::strip_jsonc(spec.json);
                let theme: vscode::VsCodeTheme = match serde_json::from_str(&stripped) {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("builtin theme {} failed to parse: {}", spec.id, e);
                        return None;
                    }
                };
                let fallback = if theme.is_dark() {
                    DARK_THEME
                } else {
                    LIGHT_THEME
                };
                let colors = vscode::vscode_to_theme_colors(&theme, &fallback);
                let info = ThemeInfo {
                    id: format!("custom:{}", spec.id),
                    name: spec.name.to_string(),
                    description: spec.description.to_string(),
                    is_dark: theme.is_dark(),
                };
                Some((info, colors))
            })
            .collect()
    })
}

/// Resolve a theme by `custom_theme_id` (without the `custom:` prefix).
pub fn builtin_colors_by_id(id: &str) -> Option<ThemeColors> {
    builtin_themes()
        .iter()
        .find(|(info, _)| info.id == format!("custom:{id}"))
        .map(|(_, colors)| *colors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_themes_parse() {
        let themes = builtin_themes();
        assert_eq!(themes.len(), SPECS.len(), "every spec must parse");
        for (info, _) in themes {
            assert!(info.id.starts_with("custom:builtin-"));
        }
    }

    #[test]
    fn default_theme_is_poimandres_dark() {
        assert_eq!(DEFAULT_THEME_ID, "builtin-poimandres-dark");
        let colors = builtin_colors_by_id(DEFAULT_THEME_ID);
        assert!(colors.is_some(), "default theme must resolve");
    }

    #[test]
    fn poimandres_dark_is_dark() {
        let themes = builtin_themes();
        let poi = themes
            .iter()
            .find(|(i, _)| i.id == "custom:builtin-poimandres-dark")
            .expect("poimandres dark must be present");
        assert!(poi.0.is_dark);
    }

    #[test]
    fn poimandres_light_is_light() {
        let themes = builtin_themes();
        let poi = themes
            .iter()
            .find(|(i, _)| i.id == "custom:builtin-poimandres-light")
            .expect("poimandres light must be present");
        assert!(!poi.0.is_dark);
    }

    #[test]
    fn dracula_loads_terminal_colors() {
        let colors = builtin_colors_by_id("builtin-dracula").unwrap();
        // Dracula's classic pink/green should be preserved
        assert_ne!(colors.term_magenta, DARK_THEME.term_magenta);
        assert_ne!(colors.term_green, DARK_THEME.term_green);
    }
}
