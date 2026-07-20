//! Theme system for formatting output colors.

use std::collections::{HashMap, HashSet};
use std::fs::read_to_string;
use std::path::PathBuf;

use arc_swap::ArcSwap;

/// The dark theme JSON content.
pub const DARK_THEME_JSON: &str = include_str!("../themes/dark.json");
/// The light theme JSON content.
pub const LIGHT_THEME_JSON: &str = include_str!("../themes/light.json");
/// The One Dark theme JSON content.
pub const ONE_DARK_THEME_JSON: &str = include_str!("../themes/one-dark.json");
/// The One Light theme JSON content.
pub const ONE_LIGHT_THEME_JSON: &str = include_str!("../themes/one-light.json");
/// The Tokyo Night theme JSON content.
pub const TOKYO_NIGHT_THEME_JSON: &str = include_str!("../themes/tokyo-night.json");
/// The Dracula theme JSON content.
pub const DRACULA_THEME_JSON: &str = include_str!("../themes/dracula.json");
/// The Poimandres Dark theme JSON content.
pub const POIMANDRES_DARK_THEME_JSON: &str = include_str!("../themes/poimandres-dark.json");
/// The Poimandres Light theme JSON content.
pub const POIMANDRES_LIGHT_THEME_JSON: &str = include_str!("../themes/poimandres-light.json");
/// The Alucard theme JSON content.
pub const ALUCARD_THEME_JSON: &str = include_str!("../themes/alucard.json");
/// Anysphere (a warm near-black dark theme).
pub const ANYSPHERE_THEME_JSON: &str = include_str!("../themes/anysphere.json");
/// Nord Midnight (a darker Nord variant).
pub const NORD_MIDNIGHT_THEME_JSON: &str = include_str!("../themes/nord-midnight.json");

/// Names of all built-in themes, in display order.
pub const BUILTIN_THEMES: &[&str] = &[
    "dark",
    "light",
    "one-dark",
    "one-light",
    "tokyo-night",
    "dracula",
    "alucard",
    "anysphere",
    "nord-midnight",
    "poimandres-dark",
    "poimandres-light",
];

/// Representation of a color value which can be either a hex/variable string or a 255-color index.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ColorValue {
    /// Hex string (e.g. "#ffffff"), variable reference name, or empty string.
    String(String),
    /// 256-color index number (0-255).
    Number(u8),
}

/// Representation of the export section of a theme json.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJson {
    /// Page background color.
    pub page_bg: Option<ColorValue>,
    /// Card background color.
    pub card_bg: Option<ColorValue>,
    /// Info box background color.
    pub info_bg: Option<ColorValue>,
}

/// Structure representing raw parsed theme JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThemeJson {
    /// Name of the theme.
    pub name: String,
    /// Variables defined in the theme.
    #[serde(default)]
    pub vars: HashMap<String, ColorValue>,
    /// Mappings of color names to color values.
    pub colors: HashMap<String, ColorValue>,
    /// Export color section.
    #[serde(default)]
    pub export: Option<ExportJson>,
}

/// Available foreground color tokens.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum_macros::EnumString,
    strum_macros::Display,
)]
#[strum(serialize_all = "camelCase")]
pub enum ThemeColor {
    /// Accent color.
    Accent,
    /// Border color.
    Border,
    /// Accent border color.
    BorderAccent,
    /// Muted border color.
    BorderMuted,
    /// Success indicator color.
    Success,
    /// Error indicator color.
    Error,
    /// Warning indicator color.
    Warning,
    /// Muted text color.
    Muted,
    /// Dim text color.
    Dim,
    /// Standard text color.
    Text,
    /// Thinking text color.
    ThinkingText,
    /// User message text color.
    UserMessageText,
    /// Custom message text color.
    CustomMessageText,
    /// Custom message label color.
    CustomMessageLabel,
    /// Tool title color.
    ToolTitle,
    /// Tool output color.
    ToolOutput,
    /// Markdown heading color.
    MdHeading,
    /// Markdown link color.
    MdLink,
    /// Markdown link URL color.
    MdLinkUrl,
    /// Markdown inline code color.
    MdCode,
    /// Markdown code block text color.
    MdCodeBlock,
    /// Markdown code block border color.
    MdCodeBlockBorder,
    /// Markdown blockquote text color.
    MdQuote,
    /// Markdown blockquote border color.
    MdQuoteBorder,
    /// Markdown horizontal rule color.
    MdHr,
    /// Markdown list bullet color.
    MdListBullet,
    /// Diff added line color.
    ToolDiffAdded,
    /// Diff removed line color.
    ToolDiffRemoved,
    /// Diff context line color.
    ToolDiffContext,
    /// Syntax highlighting comment color.
    SyntaxComment,
    /// Syntax highlighting keyword color.
    SyntaxKeyword,
    /// Syntax highlighting function color.
    SyntaxFunction,
    /// Syntax highlighting variable color.
    SyntaxVariable,
    /// Syntax highlighting string color.
    SyntaxString,
    /// Syntax highlighting number color.
    SyntaxNumber,
    /// Syntax highlighting type color.
    SyntaxType,
    /// Syntax highlighting operator color.
    SyntaxOperator,
    /// Syntax highlighting punctuation color.
    SyntaxPunctuation,
    /// Thinking off level color.
    ThinkingOff,
    /// Thinking minimal level color.
    ThinkingMinimal,
    /// Thinking low level color.
    ThinkingLow,
    /// Thinking medium level color.
    ThinkingMedium,
    /// Thinking high level color.
    ThinkingHigh,
    /// Thinking extra high level color.
    ThinkingXhigh,
    /// Bash mode color.
    BashMode,
    /// Permission-mode label: plan (and the planner agent).
    ModePlan,
    /// Permission-mode label: accept edits.
    ModeAcceptEdits,
    /// Permission-mode label: auto.
    ModeAuto,
    /// Permission-mode label: yolo.
    ModeYolo,
}

/// Available background color tokens.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum_macros::EnumString,
    strum_macros::Display,
)]
#[strum(serialize_all = "camelCase")]
pub enum ThemeBg {
    /// Selected background color.
    SelectedBg,
    /// User message background color.
    UserMessageBg,
    /// Custom message background color.
    CustomMessageBg,
    /// Tool pending background color.
    ToolPendingBg,
    /// Tool success background color.
    ToolSuccessBg,
    /// Tool error background color.
    ToolErrorBg,
}

/// Supported color output modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColorMode {
    /// Truecolor output.
    #[serde(rename = "truecolor")]
    TrueColor,
    /// 256color output.
    #[serde(rename = "256color")]
    Color256,
}

/// Structure representing export colors.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ThemeExportColors {
    /// Background color.
    pub page_bg: Option<String>,
    /// Card background color.
    pub card_bg: Option<String>,
    /// Info box background color.
    pub info_bg: Option<String>,
}

/// Immutable snapshot of resolved theme colors. Held behind an [`ArcSwap`]
/// inside [`Theme`] so a live theme switch can replace every color atomically
/// while all existing `Arc<Theme>` holders keep observing the same instance.
struct ThemeData {
    name: Option<String>,
    source_path: Option<String>,
    fg_colors: HashMap<ThemeColor, String>,
    bg_colors: HashMap<ThemeBg, String>,
    mode: ColorMode,
}

/// Theme component executing color output mapping.
///
/// The resolved colors live behind an [`ArcSwap`] so that [`Theme::replace_with`]
/// can swap the entire palette in place. Components hold a shared `Arc<Theme>`
/// and render from it live, so an in-place swap recolors them on the next paint
/// without rebuilding the component tree.
pub struct Theme {
    inner: ArcSwap<ThemeData>,
}

impl Theme {
    /// Create new theme instance with specified colors and mode.
    ///
    /// # Arguments
    /// * `fg_colors` - Foreground color configuration.
    /// * `bg_colors` - Background color configuration.
    /// * `mode` - Target output color mode.
    /// * `name` - Theme name.
    /// * `source_path` - Theme source file path.
    ///
    /// # Errors
    /// Returns error string if any color value resolution fails.
    pub fn new(
        fg_colors: HashMap<ThemeColor, ColorValue>,
        bg_colors: HashMap<ThemeBg, ColorValue>,
        mode: ColorMode,
        name: Option<String>,
        source_path: Option<String>,
    ) -> Result<Self, String> {
        let mut fg_resolved = HashMap::new();
        for (color, val) in fg_colors {
            let ansi = fg_ansi(&val, mode)?;
            fg_resolved.insert(color, ansi);
        }

        let mut bg_resolved = HashMap::new();
        for (color, val) in bg_colors {
            let ansi = bg_ansi(&val, mode)?;
            bg_resolved.insert(color, ansi);
        }

        Ok(Self {
            inner: ArcSwap::from_pointee(ThemeData {
                name,
                source_path,
                fg_colors: fg_resolved,
                bg_colors: bg_resolved,
                mode,
            }),
        })
    }

    /// The theme name, or `None` for in-memory themes built without one.
    pub fn name(&self) -> Option<String> {
        self.inner.load().name.clone()
    }

    /// The source file path the theme was loaded from, when available.
    pub fn source_path(&self) -> Option<String> {
        self.inner.load().source_path.clone()
    }

    /// Replaces this theme's resolved colors in place with those of `other`.
    ///
    /// Every existing `Arc<Theme>` clone observes the change, so a live switch
    /// recolors all components that render from a shared theme handle on their
    /// next paint. Components that bake colors into stored strings at
    /// construction are unaffected until rebuilt or invalidated.
    pub fn replace_with(&self, other: &Theme) {
        self.inner.store(other.inner.load_full());
    }

    /// Create a theme from ThemeJson representation.
    ///
    /// # Arguments
    /// * `theme_json` - ThemeJson configuration.
    /// * `mode` - Target output color mode.
    /// * `name` - Theme name.
    ///
    /// # Errors
    /// Returns error string if any color value resolution fails.
    pub fn from_json(
        theme_json: ThemeJson,
        mode: ColorMode,
        name: Option<String>,
    ) -> Result<Self, String> {
        let vars = theme_json.vars.clone();
        let mut resolved_colors = HashMap::new();
        for (k, v) in &theme_json.colors {
            let mut visited = HashSet::new();
            let resolved = resolve_var_refs(v, &vars, &mut visited)?;
            resolved_colors.insert(k.clone(), resolved);
        }

        let bg_keys: HashSet<String> = vec![
            "selectedBg".to_string(),
            "userMessageBg".to_string(),
            "customMessageBg".to_string(),
            "toolPendingBg".to_string(),
            "toolSuccessBg".to_string(),
            "toolErrorBg".to_string(),
        ]
        .into_iter()
        .collect();

        let mut fg_colors = HashMap::new();
        let mut bg_colors = HashMap::new();

        for (k, v) in resolved_colors {
            if bg_keys.contains(&k) {
                if let Ok(bg) = k.parse::<ThemeBg>() {
                    bg_colors.insert(bg, v);
                }
            } else {
                if let Ok(fg) = k.parse::<ThemeColor>() {
                    fg_colors.insert(fg, v);
                }
            }
        }

        Theme::new(fg_colors, bg_colors, mode, name, None)
    }

    /// Load a built-in theme ("dark" or "light") with auto-detected color mode.
    ///
    /// # Arguments
    /// * `name` - Theme name.
    ///
    /// # Errors
    /// Returns error string if theme load or resolution fails.
    pub fn load_default(name: &str) -> Result<Self, String> {
        let theme_json = load_theme_json(name)?;
        let mode = detect_color_mode();
        Self::from_json(theme_json, mode, Some(name.to_string()))
    }

    /// Color the input text with foreground theme color.
    ///
    /// # Arguments
    /// * `color` - Target foreground color token.
    /// * `text` - Text to format.
    pub fn fg(&self, color: ThemeColor, text: &str) -> String {
        let data = self.inner.load();
        let ansi = data.fg_colors.get(&color).map(|s| s.as_str()).unwrap_or("");
        format!("{}{}\x1b[39m", ansi, text)
    }

    /// Color the input text with background theme color.
    ///
    /// # Arguments
    /// * `color` - Target background color token.
    /// * `text` - Text to format.
    pub fn bg(&self, color: ThemeBg, text: &str) -> String {
        let data = self.inner.load();
        let ansi = data.bg_colors.get(&color).map(|s| s.as_str()).unwrap_or("");
        format!("{}{}\x1b[49m", ansi, text)
    }

    /// Style text as bold.
    ///
    /// # Arguments
    /// * `text` - Text to format.
    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{}\x1b[22m", text)
    }

    /// Style text as italic.
    ///
    /// # Arguments
    /// * `text` - Text to format.
    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{}\x1b[23m", text)
    }

    /// Style text as underline.
    ///
    /// # Arguments
    /// * `text` - Text to format.
    pub fn underline(&self, text: &str) -> String {
        format!("\x1b[4m{}\x1b[24m", text)
    }

    /// Style text as inverse.
    ///
    /// # Arguments
    /// * `text` - Text to format.
    pub fn inverse(&self, text: &str) -> String {
        format!("\x1b[7m{}\x1b[27m", text)
    }

    /// Style text as strikethrough.
    ///
    /// # Arguments
    /// * `text` - Text to format.
    pub fn strikethrough(&self, text: &str) -> String {
        format!("\x1b[9m{}\x1b[29m", text)
    }

    /// Get raw ANSI escape sequence for foreground color.
    ///
    /// # Arguments
    /// * `color` - Target foreground color token.
    pub fn get_fg_ansi(&self, color: ThemeColor) -> String {
        self.inner.load().fg_colors.get(&color).cloned().unwrap_or_default()
    }

    /// Get raw ANSI escape sequence for background color.
    ///
    /// # Arguments
    /// * `color` - Target background color token.
    pub fn get_bg_ansi(&self, color: ThemeBg) -> String {
        self.inner.load().bg_colors.get(&color).cloned().unwrap_or_default()
    }

    /// Get active color output mode.
    pub fn get_color_mode(&self) -> ColorMode {
        self.inner.load().mode
    }

    /// Get border styling color function for thinking block based on level.
    ///
    /// # Arguments
    /// * `level` - Thinking level identifier.
    pub fn get_thinking_border_color(&self, level: &str) -> Box<dyn Fn(&str) -> String + '_> {
        let color = match level {
            "off" => ThemeColor::ThinkingOff,
            "minimal" => ThemeColor::ThinkingMinimal,
            "low" => ThemeColor::ThinkingLow,
            "medium" => ThemeColor::ThinkingMedium,
            "high" => ThemeColor::ThinkingHigh,
            "xhigh" => ThemeColor::ThinkingXhigh,
            _ => ThemeColor::ThinkingOff,
        };
        Box::new(move |s: &str| self.fg(color, s))
    }

    /// Get border styling color function for bash execution block.
    pub fn get_bash_mode_border_color(&self) -> Box<dyn Fn(&str) -> String + '_> {
        Box::new(move |s: &str| self.fg(ThemeColor::BashMode, s))
    }
}

/// Detect the active color mode based on environment variables.
pub fn detect_color_mode() -> ColorMode {
    if let Ok(colorterm) = std::env::var("COLORTERM")
        && (colorterm == "truecolor" || colorterm == "24bit")
    {
        return ColorMode::TrueColor;
    }
    if std::env::var("WT_SESSION").is_ok() {
        return ColorMode::TrueColor;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term == "dumb" || term.is_empty() || term == "linux" {
        return ColorMode::Color256;
    }
    if let Ok(term_program) = std::env::var("TERM_PROGRAM")
        && term_program == "Apple_Terminal"
    {
        return ColorMode::Color256;
    }
    if term == "screen" || term.starts_with("screen-") || term.starts_with("screen.") {
        return ColorMode::Color256;
    }
    ColorMode::TrueColor
}

/// Convert hex color string to rgb components.
///
/// # Arguments
/// * `hex` - Hex format color string.
///
/// # Errors
/// Returns error if format does not match six characters or contains non-hex digits.
pub fn hex_to_rgb(hex: &str) -> Result<(u8, u8, u8), String> {
    let cleaned = hex.replace('#', "");
    if cleaned.len() != 6 {
        return Err(format!("Invalid hex color: {}", hex));
    }
    let r = u8::from_str_radix(&cleaned[0..2], 16)
        .map_err(|_| format!("Invalid hex color: {}", hex))?;
    let g = u8::from_str_radix(&cleaned[2..4], 16)
        .map_err(|_| format!("Invalid hex color: {}", hex))?;
    let b = u8::from_str_radix(&cleaned[4..6], 16)
        .map_err(|_| format!("Invalid hex color: {}", hex))?;
    Ok((r, g, b))
}

const CUBE_VALUES: [i32; 6] = [0, 95, 135, 175, 215, 255];

fn find_closest_cube_index(value: i32) -> usize {
    let mut min_dist = i32::MAX;
    let mut min_idx = 0;
    for (i, &cube_val) in CUBE_VALUES.iter().enumerate() {
        let dist = (value - cube_val).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

fn find_closest_gray_index(gray: i32) -> usize {
    let mut min_dist = i32::MAX;
    let mut min_idx = 0;
    for i in 0..24 {
        let gray_val = 8 + i * 10;
        let dist = (gray - gray_val).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i as usize;
        }
    }
    min_idx
}

fn color_distance(r1: f64, g1: f64, b1: f64, r2: f64, g2: f64, b2: f64) -> f64 {
    let dr = r1 - r2;
    let dg = g1 - g2;
    let db = b1 - b2;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

/// Map RGB values to closest 256-color index.
///
/// # Arguments
/// * `r` - Red channel.
/// * `g` - Green channel.
/// * `b` - Blue channel.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let r_i = r as i32;
    let g_i = g as i32;
    let b_i = b as i32;

    let r_idx = find_closest_cube_index(r_i);
    let g_idx = find_closest_cube_index(g_i);
    let b_idx = find_closest_cube_index(b_i);

    let cube_r = CUBE_VALUES[r_idx];
    let cube_g = CUBE_VALUES[g_idx];
    let cube_b = CUBE_VALUES[b_idx];

    let cube_index = 16 + 36 * r_idx + 6 * g_idx + b_idx;
    let cube_dist = color_distance(
        r as f64,
        g as f64,
        b as f64,
        cube_r as f64,
        cube_g as f64,
        cube_b as f64,
    );

    let gray = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round() as i32;
    let gray_idx = find_closest_gray_index(gray);
    let gray_value = 8 + gray_idx as i32 * 10;
    let gray_index = 232 + gray_idx;
    let gray_dist = color_distance(
        r as f64,
        g as f64,
        b as f64,
        gray_value as f64,
        gray_value as f64,
        gray_value as f64,
    );

    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let spread = max_c - min_c;

    if spread < 10 && gray_dist < cube_dist {
        return gray_index as u8;
    }

    cube_index as u8
}

/// Convert hex color string to 256-color index.
///
/// # Arguments
/// * `hex` - Hex format color string.
///
/// # Errors
/// Returns error if hex parser fails.
pub fn hex_to_256(hex: &str) -> Result<u8, String> {
    let (r, g, b) = hex_to_rgb(hex)?;
    Ok(rgb_to_256(r, g, b))
}

/// Convert 256-color index to closest hex string.
///
/// # Arguments
/// * `index` - 256 color index.
pub fn ansi_256_to_hex(index: u8) -> String {
    let basic_colors = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
        "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if index < 16 {
        return basic_colors[index as usize].to_string();
    }

    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_hex = |n: u8| {
            let val = if n == 0 { 0 } else { 55 + n * 40 };
            format!("{:02x}", val)
        };
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }

    let gray = 8 + (index - 232) * 10;
    let gray_hex = format!("{:02x}", gray);
    format!("#{}{}{}", gray_hex, gray_hex, gray_hex)
}

/// Format the color value into standard foreground ANSI escape sequence.
///
/// # Arguments
/// * `color` - Color value token.
/// * `mode` - Target color mode.
///
/// # Errors
/// Returns error if color string is invalid.
pub fn fg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, String> {
    match color {
        ColorValue::String(s) if s.is_empty() => Ok("\x1b[39m".to_string()),
        ColorValue::Number(num) => Ok(format!("\x1b[38;5;{}m", num)),
        ColorValue::String(s) if s.starts_with('#') => {
            if mode == ColorMode::TrueColor {
                let (r, g, b) = hex_to_rgb(s)?;
                Ok(format!("\x1b[38;2;{};{};{}m", r, g, b))
            } else {
                let index = hex_to_256(s)?;
                Ok(format!("\x1b[38;5;{}m", index))
            }
        }
        _ => Err(format!("Invalid color value: {:?}", color)),
    }
}

/// Format the color value into standard background ANSI escape sequence.
///
/// # Arguments
/// * `color` - Color value token.
/// * `mode` - Target color mode.
///
/// # Errors
/// Returns error if color string is invalid.
pub fn bg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, String> {
    match color {
        ColorValue::String(s) if s.is_empty() => Ok("\x1b[49m".to_string()),
        ColorValue::Number(num) => Ok(format!("\x1b[48;5;{}m", num)),
        ColorValue::String(s) if s.starts_with('#') => {
            if mode == ColorMode::TrueColor {
                let (r, g, b) = hex_to_rgb(s)?;
                Ok(format!("\x1b[48;2;{};{};{}m", r, g, b))
            } else {
                let index = hex_to_256(s)?;
                Ok(format!("\x1b[48;5;{}m", index))
            }
        }
        _ => Err(format!("Invalid color value: {:?}", color)),
    }
}

/// Resolve variable references in color value mappings.
///
/// # Arguments
/// * `value` - Color value to resolve.
/// * `vars` - Map of variable name keys to values.
/// * `visited` - Set to detect circular resolution.
///
/// # Errors
/// Returns error if circular reference is detected or referenced variable does not exist.
pub fn resolve_var_refs(
    value: &ColorValue,
    vars: &HashMap<String, ColorValue>,
    visited: &mut HashSet<String>,
) -> Result<ColorValue, String> {
    match value {
        ColorValue::Number(n) => Ok(ColorValue::Number(*n)),
        ColorValue::String(s) => {
            if s.is_empty() || s.starts_with('#') {
                return Ok(ColorValue::String(s.clone()));
            }
            if visited.contains(s) {
                return Err(format!("Circular variable reference detected: {}", s));
            }
            if !vars.contains_key(s) {
                return Err(format!("Variable reference not found: {}", s));
            }
            visited.insert(s.clone());
            resolve_var_refs(&vars[s], vars, visited)
        }
    }
}

/// Get the configuration directory.
pub fn get_agent_dir() -> PathBuf {
    if let Ok(env_dir) = std::env::var("NOTAGENT_CODING_AGENT_DIR") {
        return PathBuf::from(env_dir);
    }
    if let Some(home) = dirs::home_dir() {
        home.join(".notagent").join("agent")
    } else {
        PathBuf::from(".notagent").join("agent")
    }
}

/// Get the user custom themes directory.
pub fn get_custom_themes_dir() -> PathBuf {
    get_agent_dir().join("themes")
}

/// List all available theme names: the built-ins (in display order) followed by
/// any user custom themes found as `*.json` files in the custom themes
/// directory. Built-ins shadow custom files of the same name and are not
/// duplicated; custom names are sorted alphabetically.
pub fn list_available_themes() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_THEMES.iter().map(|s| s.to_string()).collect();
    if let Ok(entries) = std::fs::read_dir(get_custom_themes_dir()) {
        let mut custom: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| stem.to_string())
                } else {
                    None
                }
            })
            .filter(|name| !BUILTIN_THEMES.contains(&name.as_str()))
            .collect();
        custom.sort();
        names.extend(custom);
    }
    names
}

/// Load theme raw JSON representation by name.
///
/// # Arguments
/// * `name` - Target theme name.
///
/// # Errors
/// Returns error if theme file is not found or parsing fails.
pub fn load_theme_json(name: &str) -> Result<ThemeJson, String> {
    if name == "dark" {
        return serde_json::from_str(DARK_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "light" {
        return serde_json::from_str(LIGHT_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "one-dark" {
        return serde_json::from_str(ONE_DARK_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "one-light" {
        return serde_json::from_str(ONE_LIGHT_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "tokyo-night" {
        return serde_json::from_str(TOKYO_NIGHT_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "dracula" {
        return serde_json::from_str(DRACULA_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "poimandres-dark" {
        return serde_json::from_str(POIMANDRES_DARK_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "poimandres-light" {
        return serde_json::from_str(POIMANDRES_LIGHT_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "alucard" {
        return serde_json::from_str(ALUCARD_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "anysphere" {
        return serde_json::from_str(ANYSPHERE_THEME_JSON).map_err(|e| e.to_string());
    }
    if name == "nord-midnight" {
        return serde_json::from_str(NORD_MIDNIGHT_THEME_JSON).map_err(|e| e.to_string());
    }

    let custom_themes_dir = get_custom_themes_dir();
    let theme_path = custom_themes_dir.join(format!("{}.json", name));
    if !theme_path.exists() {
        return Err(format!("Theme not found: {}", name));
    }
    let content = read_to_string(&theme_path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

/// Load theme export color values by theme name.
///
/// # Arguments
/// * `theme_name` - Optional theme name, defaults to "dark".
pub fn get_theme_export_colors(theme_name: Option<&str>) -> ThemeExportColors {
    let name = theme_name.unwrap_or("dark");
    match load_theme_json(name) {
        Ok(theme_json) => {
            let export_section = match theme_json.export {
                Some(exp) => exp,
                None => return ThemeExportColors::default(),
            };
            let vars = theme_json.vars;
            let resolve = |value: &Option<ColorValue>| -> Option<String> {
                let val = value.as_ref()?;
                let mut visited = HashSet::new();
                let resolved = resolve_var_refs(val, &vars, &mut visited).ok()?;
                match resolved {
                    ColorValue::Number(num) => Some(ansi_256_to_hex(num)),
                    ColorValue::String(s) => {
                        if s.is_empty() {
                            None
                        } else {
                            Some(s)
                        }
                    }
                }
            };
            ThemeExportColors {
                page_bg: resolve(&export_section.page_bg),
                card_bg: resolve(&export_section.card_bg),
                info_bg: resolve(&export_section.info_bg),
            }
        }
        Err(_) => ThemeExportColors::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs::{create_dir_all, write};
    use std::sync::Mutex;
    use tempfile::tempdir;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_resolves_export_variable_references() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let setup = tempdir().unwrap();
        let agent_dir = setup.path().join("agent");
        let themes_dir = agent_dir.join("themes");
        create_dir_all(&themes_dir).unwrap();

        unsafe {
            std::env::set_var("NOTAGENT_CODING_AGENT_DIR", agent_dir.to_str().unwrap());
        }

        let mut dark_json: ThemeJson = serde_json::from_str(DARK_THEME_JSON).unwrap();
        dark_json.name = "custom-export-vars".to_string();
        dark_json.vars.insert(
            "pageBgVar".to_string(),
            ColorValue::String("#112233".to_string()),
        );
        dark_json.vars.insert(
            "pageBgAlias".to_string(),
            ColorValue::String("pageBgVar".to_string()),
        );
        dark_json.vars.insert(
            "infoBgVar".to_string(),
            ColorValue::String("#445566".to_string()),
        );
        dark_json.vars.insert(
            "cardBgVar".to_string(),
            ColorValue::String("#223344".to_string()),
        );

        dark_json.export = Some(ExportJson {
            page_bg: Some(ColorValue::String("pageBgAlias".to_string())),
            card_bg: Some(ColorValue::String("cardBgVar".to_string())),
            info_bg: Some(ColorValue::String("infoBgVar".to_string())),
        });

        let json_str = serde_json::to_string_pretty(&dark_json).unwrap();
        write(themes_dir.join("custom-export-vars.json"), json_str).unwrap();

        let actual = get_theme_export_colors(Some("custom-export-vars"));
        let expected = ThemeExportColors {
            page_bg: Some("#112233".to_string()),
            card_bg: Some("#223344".to_string()),
            info_bg: Some("#445566".to_string()),
        };

        assert_eq!(actual, expected);

        unsafe {
            std::env::remove_var("NOTAGENT_CODING_AGENT_DIR");
        }
    }

    #[test]
    fn test_resolves_recursive_vars_and_converts_256_color_export_values_to_hex() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let setup = tempdir().unwrap();
        let agent_dir = setup.path().join("agent");
        let themes_dir = agent_dir.join("themes");
        create_dir_all(&themes_dir).unwrap();

        unsafe {
            std::env::set_var("NOTAGENT_CODING_AGENT_DIR", agent_dir.to_str().unwrap());
        }

        let mut dark_json: ThemeJson = serde_json::from_str(DARK_THEME_JSON).unwrap();
        dark_json.name = "custom-export-recursive".to_string();
        dark_json.vars.insert(
            "deepPageBg".to_string(),
            ColorValue::String("#abcdef".to_string()),
        );
        dark_json.vars.insert(
            "pageBgAlias".to_string(),
            ColorValue::String("deepPageBg".to_string()),
        );
        dark_json
            .vars
            .insert("cardBgAnsi".to_string(), ColorValue::Number(24));

        dark_json.export = Some(ExportJson {
            page_bg: Some(ColorValue::String("pageBgAlias".to_string())),
            card_bg: Some(ColorValue::String("cardBgAnsi".to_string())),
            info_bg: Some(ColorValue::String("".to_string())),
        });

        let json_str = serde_json::to_string_pretty(&dark_json).unwrap();
        write(themes_dir.join("custom-export-recursive.json"), json_str).unwrap();

        let actual = get_theme_export_colors(Some("custom-export-recursive"));
        let expected = ThemeExportColors {
            page_bg: Some("#abcdef".to_string()),
            card_bg: Some("#005f87".to_string()),
            info_bg: None,
        };

        assert_eq!(actual, expected);

        unsafe {
            std::env::remove_var("NOTAGENT_CODING_AGENT_DIR");
        }
    }

    #[test]
    fn test_all_builtin_themes_load_and_resolve() {
        for name in BUILTIN_THEMES {
            let theme_json = load_theme_json(name)
                .unwrap_or_else(|e| panic!("built-in theme '{name}' failed to parse: {e}"));
            assert_eq!(&theme_json.name, name, "theme '{name}' has mismatched name field");
            Theme::from_json(theme_json, ColorMode::TrueColor, Some(name.to_string()))
                .unwrap_or_else(|e| panic!("built-in theme '{name}' failed to resolve: {e}"));
        }
    }

    #[test]
    fn test_theme_formatting() {
        let dark_json: ThemeJson = serde_json::from_str(DARK_THEME_JSON).unwrap();

        let vars = dark_json.vars.clone();
        let mut resolved_colors = HashMap::new();
        for (k, v) in &dark_json.colors {
            let mut visited = HashSet::new();
            let resolved = resolve_var_refs(v, &vars, &mut visited).unwrap();
            resolved_colors.insert(k.clone(), resolved);
        }

        let bg_keys: HashSet<String> = vec![
            "selectedBg".to_string(),
            "userMessageBg".to_string(),
            "customMessageBg".to_string(),
            "toolPendingBg".to_string(),
            "toolSuccessBg".to_string(),
            "toolErrorBg".to_string(),
        ]
        .into_iter()
        .collect();

        let mut fg_colors = HashMap::new();
        let mut bg_colors = HashMap::new();

        for (k, v) in resolved_colors {
            if bg_keys.contains(&k) {
                if let Ok(bg) = k.parse::<ThemeBg>() {
                    bg_colors.insert(bg, v);
                }
            } else {
                if let Ok(fg) = k.parse::<ThemeColor>() {
                    fg_colors.insert(fg, v);
                }
            }
        }

        // TrueColor Mode
        let theme_tc = Theme::new(
            fg_colors.clone(),
            bg_colors.clone(),
            ColorMode::TrueColor,
            Some("dark".to_string()),
            None,
        )
        .unwrap();

        // success maps to #8a8bbe in dark theme (r=138, g=139, b=190)
        assert_eq!(
            theme_tc.fg(ThemeColor::Success, "hello"),
            "\x1b[38;2;138;139;190mhello\x1b[39m"
        );
        // toolSuccessBg maps to #283228 in dark theme (r=40, g=50, b=40)
        assert_eq!(
            theme_tc.bg(ThemeBg::ToolSuccessBg, "world"),
            "\x1b[48;2;40;50;40mworld\x1b[49m"
        );

        // Color256 Mode
        let theme_256 = Theme::new(
            fg_colors,
            bg_colors,
            ColorMode::Color256,
            Some("dark".to_string()),
            None,
        )
        .unwrap();

        // #8a8bbe should map to 256color index 103
        assert_eq!(
            theme_256.fg(ThemeColor::Success, "hello"),
            "\x1b[38;5;103mhello\x1b[39m"
        );

        // Styling
        assert_eq!(theme_tc.bold("bold"), "\x1b[1mbold\x1b[22m");
        assert_eq!(theme_tc.italic("italic"), "\x1b[3mitalic\x1b[23m");
    }
}
