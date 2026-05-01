//! VS Code theme compatibility.
//!
//! Loads VS Code color theme JSON files (`*-color-theme.json`) and maps
//! them to Vryn's `ThemeColors`. Users can download a theme from the
//! VS Code extension source, drop the JSON into `~/.config/vryn-ws/themes/`,
//! and it will appear in the theme picker.
//!
//! Supported:
//! - The `colors` dictionary with dot-notation keys (`editor.background`,
//!   `terminal.ansiRed`, etc).
//! - `include` for theme inheritance (e.g. `Dark+` includes `dark_vs`).
//! - Alpha suffix in colors (`#RRGGBBAA`) — alpha is stripped.
//! - 3-digit hex shorthand (`#f0a` → `#ff00aa`).
//! - JSONC: line (`//`) and block (`/* */`) comments.
//!
//! Ignored (not in scope): `tokenColors`, `semanticTokenColors` (Vryn has
//! no editor/file viewer yet).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use vryn_core::theme::ThemeColors;

const MAX_INCLUDE_DEPTH: u8 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct VsCodeTheme {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub include: Option<String>,
    // Real-world VS Code themes occasionally set a color key to `null`
    // (Dracula does this to blank out defaults). Accept `Option<String>`
    // and filter out `None` values on read.
    #[serde(default, deserialize_with = "deserialize_colors")]
    pub colors: HashMap<String, String>,
}

fn deserialize_colors<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: HashMap<String, Option<String>> = HashMap::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(k, v)| v.map(|value| (k, value)))
        .collect())
}

impl VsCodeTheme {
    pub fn is_dark(&self) -> bool {
        match self.kind.as_deref() {
            Some("light") | Some("hc-light") => return false,
            Some("dark") | Some("hc-black") => return true,
            _ => {}
        }
        // `type` is optional (e.g. Solarized Light omits it). Infer from the
        // editor background luminance — dark bg means dark theme.
        let bg = self
            .colors
            .get("editor.background")
            .or_else(|| self.colors.get("sideBar.background"))
            .or_else(|| self.colors.get("panel.background"))
            .and_then(|s| parse_vscode_color(s));
        match bg {
            Some(rgb) => {
                let r = (rgb >> 16) & 0xff;
                let g = (rgb >> 8) & 0xff;
                let b = rgb & 0xff;
                // Perceived luminance (Rec. 601). > 128 → light theme.
                let luma = (r * 299 + g * 587 + b * 114) / 1000;
                luma <= 128
            }
            None => true,
        }
    }
}

/// Detect whether a parsed JSON value looks like a VS Code theme.
/// Heuristic: `colors` object contains at least one dot-notation key.
/// Vryn native themes use flat keys like `bg_primary`.
pub fn is_vscode_schema(value: &serde_json::Value) -> bool {
    if let Some(colors) = value.get("colors").and_then(|c| c.as_object())
        && colors.keys().any(|k| k.contains('.'))
    {
        return true;
    }
    value.get("tokenColors").is_some()
        || value.get("semanticTokenColors").is_some()
        || value.get("include").is_some()
}

/// Parse a VS Code hex color into u32 RGB. Strips alpha if present.
pub fn parse_vscode_color(s: &str) -> Option<u32> {
    parse_vscode_color_rgba(s).map(|(rgb, _)| rgb)
}

/// Parse a VS Code hex color into (rgb, alpha). Alpha is 0xff for colors
/// written without an alpha suffix. Many VS Code themes (Poimandres,
/// Dracula) rely on alpha blending for subtle UI tints, so the caller
/// needs to composite the color against a backdrop.
pub fn parse_vscode_color_rgba(s: &str) -> Option<(u32, u8)> {
    let s = s.trim().trim_start_matches('#');
    let expand = |c: &str| -> Option<u8> { u8::from_str_radix(&c.repeat(2), 16).ok() };
    match s.len() {
        3 => {
            let r = expand(&s[0..1])?;
            let g = expand(&s[1..2])?;
            let b = expand(&s[2..3])?;
            Some((((r as u32) << 16) | ((g as u32) << 8) | b as u32, 0xff))
        }
        4 => {
            let r = expand(&s[0..1])?;
            let g = expand(&s[1..2])?;
            let b = expand(&s[2..3])?;
            let a = expand(&s[3..4])?;
            Some((((r as u32) << 16) | ((g as u32) << 8) | b as u32, a))
        }
        6 => u32::from_str_radix(s, 16).ok().map(|rgb| (rgb, 0xff)),
        8 => {
            let rgb = u32::from_str_radix(&s[..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..], 16).ok()?;
            Some((rgb, a))
        }
        _ => None,
    }
}

/// Alpha-blend a foreground color over a backdrop. Returns opaque RGB.
pub fn blend_over(fg: u32, alpha: u8, bg: u32) -> u32 {
    if alpha == 0xff {
        return fg;
    }
    if alpha == 0 {
        return bg;
    }
    let a = alpha as u32;
    let inv = 255 - a;
    let r = ((fg >> 16) & 0xff) * a + ((bg >> 16) & 0xff) * inv;
    let g = ((fg >> 8) & 0xff) * a + ((bg >> 8) & 0xff) * inv;
    let b = (fg & 0xff) * a + (bg & 0xff) * inv;
    (((r + 127) / 255) << 16) | (((g + 127) / 255) << 8) | ((b + 127) / 255)
}

/// Rec. 601 luminance (0..255).
fn luma(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    (r * 299 + g * 587 + b * 114) / 1000
}

/// VS Code's `panel.border` convention is "dark-black-at-low-alpha". When
/// blended over a dark backdrop this produces a line that is slightly
/// *darker* than the backdrop — essentially invisible. Zed's native themes
/// (One, Ayu, Gruvbox) break from this and draw borders as fully opaque
/// *lighter-than-bg* lines.
///
/// If a mapped border doesn't have enough contrast against `bg`, derive a
/// Zed-style visible border by blending `text_primary` onto `bg` at a
/// low alpha — this lightens dark themes and darkens light ones.
fn ensure_visible_border(candidate: u32, bg: u32, text: u32) -> u32 {
    const MIN_DELTA: u32 = 12; // luma units — anything smaller reads as invisible
    let bg_luma = luma(bg);
    let cand_luma = luma(candidate);
    let delta = bg_luma.abs_diff(cand_luma);
    if delta >= MIN_DELTA {
        return candidate;
    }
    // ~15% of the primary text color on top of the backdrop.
    blend_over(text, 0x26, bg)
}

/// Like `ensure_visible_border`, but also caps the *upper* contrast bound.
/// Passive dividers (the main `border` slot — tab-bar bottom, panel edges,
/// sidebar boundaries) should read as quiet separators, not accent rails.
/// When a theme parks its accent in `panel.border` (Dracula: `#BD93F9`),
/// this collapses the too-loud candidate to a neutral text-over-bg blend.
/// Themes with a properly subtle `panel.border` (Poimandres' `#00000030`)
/// land inside the contrast window and pass through untouched.
fn ensure_subtle_border(candidate: u32, bg: u32, text: u32) -> u32 {
    const MIN_DELTA: u32 = 12;
    const MAX_DELTA: u32 = 60;
    let bg_luma = luma(bg);
    let cand_luma = luma(candidate);
    let delta = bg_luma.abs_diff(cand_luma);
    if (MIN_DELTA..=MAX_DELTA).contains(&delta) {
        return candidate;
    }
    blend_over(text, 0x26, bg)
}

/// Normalize JSONC into strict JSON: strip `//` and `/* */` comments,
/// and remove trailing commas before `}` or `]`. String literals are respected.
pub(crate) fn strip_jsonc(input: &str) -> String {
    let without_comments = strip_comments(input);
    strip_trailing_commas(&without_comments)
}

fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Load a VS Code theme JSON file, resolving `include` chain.
pub fn load_with_includes(path: &Path) -> Result<VsCodeTheme, String> {
    load_recursive(path, 0)
}

fn load_recursive(path: &Path, depth: u8) -> Result<VsCodeTheme, String> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(format!(
            "include chain exceeds max depth ({MAX_INCLUDE_DEPTH}) at {}",
            path.display()
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let stripped = strip_jsonc(&raw);
    let mut theme: VsCodeTheme =
        serde_json::from_str(&stripped).map_err(|e| format!("parse {}: {e}", path.display()))?;

    if let Some(include_rel) = theme.include.clone() {
        let parent_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let include_path = parent_dir.join(&include_rel);
        let parent = load_recursive(&include_path, depth + 1)?;
        let mut merged = parent.colors;
        for (k, v) in theme.colors.drain() {
            merged.insert(k, v);
        }
        theme.colors = merged;
        if theme.name.is_none() {
            theme.name = parent.name;
        }
        if theme.kind.is_none() {
            theme.kind = parent.kind;
        }
    }
    Ok(theme)
}

/// Map a VS Code theme to Vryn's flat `ThemeColors`, falling back to `fallback`
/// for any Vryn field that has no matching VS Code key.
///
/// VS Code themes heavily rely on alpha-blended colors for subtle UI tints
/// (selections, hovers, borders). Because Vryn's `ThemeColors` stores
/// opaque `u32` RGB, we pre-blend every alpha'd value against the resolved
/// primary background so that the on-screen result matches VS Code's render.
pub fn vscode_to_theme_colors(theme: &VsCodeTheme, fallback: &ThemeColors) -> ThemeColors {
    // Resolve bg_primary first so we can blend other alpha colors against it.
    let bg_primary_raw = |keys: &[&str]| -> Option<u32> {
        for k in keys {
            if let Some(v) = theme.colors.get(*k)
                && let Some((rgb, a)) = parse_vscode_color_rgba(v)
                && a > 0
            {
                return Some(blend_over(rgb, a, fallback.bg_primary));
            }
        }
        None
    };
    let bg_primary = bg_primary_raw(&[
        "editor.background",
        "sideBar.background",
        "panel.background",
        "activityBar.background",
    ])
    .unwrap_or(fallback.bg_primary);

    // Look up a color by a priority chain. Skips entries whose alpha is 0
    // (VS Code themes sometimes set `focusBorder: #00000000` to turn it off).
    // Non-opaque colors are blended against `bg_primary`.
    let look = |keys: &[&str]| -> Option<u32> {
        for k in keys {
            if let Some(v) = theme.colors.get(*k)
                && let Some((rgb, a)) = parse_vscode_color_rgba(v)
                && a > 0
            {
                return Some(blend_over(rgb, a, bg_primary));
            }
        }
        None
    };

    // Like `look`, but returns the raw RGB without blending against the
    // background. Use for fields the renderer re-applies its own alpha to —
    // otherwise alpha gets multiplied twice and the colour washes out.
    let look_raw = |keys: &[&str]| -> Option<u32> {
        for k in keys {
            if let Some(v) = theme.colors.get(*k)
                && let Some((rgb, _a)) = parse_vscode_color_rgba(v)
            {
                return Some(rgb);
            }
        }
        None
    };

    // Primary text — resolved early so we can derive contrast-visible borders from it.
    let text_primary = look(&["foreground", "editor.foreground"]).unwrap_or(fallback.text_primary);
    let text_secondary = look(&[
        "tab.inactiveForeground",
        "sideBar.foreground",
        "descriptionForeground",
    ])
    .unwrap_or(fallback.text_secondary);
    // Zed uses `tab.inactiveForeground` for text_muted (see theme_importer).
    let text_muted = look(&[
        "tab.inactiveForeground",
        "descriptionForeground",
        "disabledForeground",
    ])
    .unwrap_or(fallback.text_muted);

    let bg_secondary = look(&[
        "sideBar.background",
        "sideBarSectionHeader.background",
        "tab.inactiveBackground",
        "panel.background",
    ])
    .unwrap_or(fallback.bg_secondary);
    let bg_header = look(&["titleBar.activeBackground", "activityBar.background"])
        .unwrap_or(fallback.bg_header);
    // Selection: prefer list-style fill (subtle, used for selected rows)
    // over editor.selectionBackground which is often a vivid text highlight.
    let bg_selection = look(&[
        "list.activeSelectionBackground",
        "list.inactiveSelectionBackground",
        "editor.selectionBackground",
    ])
    .unwrap_or(fallback.bg_selection);
    let bg_hover =
        look(&["list.hoverBackground", "toolbar.hoverBackground"]).unwrap_or(fallback.bg_hover);

    // Separator lines. VS Code themes encode `panel.border` as dark-at-low-alpha,
    // which blended over a dark bg produces a darker-than-bg line — invisible in
    // practice. Zed's native themes draw borders as opaque *lighter* lines, so we
    // post-correct with `ensure_visible_border`.
    let border_raw = look(&[
        "panel.border",
        "editorGroup.border",
        "sideBar.border",
        "titleBar.border",
        "tab.border",
    ])
    .unwrap_or(fallback.border);
    let border = ensure_subtle_border(border_raw, bg_primary, text_primary);
    // "Active" accent: the color drawn for selected tabs, splitter hovers,
    // cursor-row indicators, primary action borders. Prefer explicit accent
    // surfaces over `focusBorder`: themes like One Dark and Alucard use
    // `focusBorder` for neutral focus outlines, while `sash.hoverBorder` or
    // active tab/activity colors carry the actual accent.
    let border_active_raw = look(&[
        "sash.hoverBorder",
        "tab.activeBorderTop",
        "activityBar.activeBorder",
        "panelTitle.activeBorder",
        "progressBar.background",
        "textLink.foreground",
        "editorLink.activeForeground",
        "inputOption.activeBorder",
        "tab.activeBorder",
        "focusBorder",
    ])
    .unwrap_or(fallback.border_active);
    let border_active = ensure_visible_border(border_active_raw, bg_primary, text_primary);
    // Keyboard-focus ring. Many themes set `focusBorder` to fully transparent —
    // in that case reuse the active accent so focus is still visible.
    let border_focused_raw = look(&["focusBorder", "tab.activeBorder"]).unwrap_or(border_active);
    let border_focused = ensure_visible_border(border_focused_raw, bg_primary, text_primary);
    let border_bell = look(&[
        "notificationsWarningIcon.foreground",
        "list.warningForeground",
        "editorWarning.foreground",
    ])
    .unwrap_or(fallback.border_bell);
    let border_idle =
        look(&["descriptionForeground", "disabledForeground"]).unwrap_or(fallback.border_idle);

    // Text selection inside a terminal pane.
    let selection_bg = look(&[
        "terminal.selectionBackground",
        "editor.selectionBackground",
        "list.activeSelectionBackground",
    ])
    .unwrap_or(fallback.selection_bg);
    let selection_fg = look(&[
        "terminal.selectionForeground",
        "list.activeSelectionForeground",
        "editor.foreground",
    ])
    .unwrap_or(fallback.selection_fg);

    let search_match_bg =
        look(&["editor.findMatchHighlightBackground"]).unwrap_or(fallback.search_match_bg);
    let search_current_bg =
        look(&["editor.findMatchBackground"]).unwrap_or(fallback.search_current_bg);

    let term_black = look(&["terminal.ansiBlack"]).unwrap_or(fallback.term_black);
    let term_red = look(&["terminal.ansiRed"]).unwrap_or(fallback.term_red);
    let term_green = look(&["terminal.ansiGreen"]).unwrap_or(fallback.term_green);
    let term_yellow = look(&["terminal.ansiYellow"]).unwrap_or(fallback.term_yellow);
    let term_blue = look(&["terminal.ansiBlue"]).unwrap_or(fallback.term_blue);
    let term_magenta = look(&["terminal.ansiMagenta"]).unwrap_or(fallback.term_magenta);
    let term_cyan = look(&["terminal.ansiCyan"]).unwrap_or(fallback.term_cyan);
    let term_white = look(&["terminal.ansiWhite"]).unwrap_or(fallback.term_white);
    let term_bright_black =
        look(&["terminal.ansiBrightBlack"]).unwrap_or(fallback.term_bright_black);
    let term_bright_red = look(&["terminal.ansiBrightRed"]).unwrap_or(fallback.term_bright_red);
    let term_bright_green =
        look(&["terminal.ansiBrightGreen"]).unwrap_or(fallback.term_bright_green);
    let term_bright_yellow =
        look(&["terminal.ansiBrightYellow"]).unwrap_or(fallback.term_bright_yellow);
    let term_bright_blue = look(&["terminal.ansiBrightBlue"]).unwrap_or(fallback.term_bright_blue);
    let term_bright_magenta =
        look(&["terminal.ansiBrightMagenta"]).unwrap_or(fallback.term_bright_magenta);
    let term_bright_cyan = look(&["terminal.ansiBrightCyan"]).unwrap_or(fallback.term_bright_cyan);
    let term_bright_white =
        look(&["terminal.ansiBrightWhite"]).unwrap_or(fallback.term_bright_white);

    // Dim ANSI colors — VSCode JSON has no dim slots, so always the theme fallback.
    let term_dim_black = fallback.term_dim_black;
    let term_dim_red = fallback.term_dim_red;
    let term_dim_green = fallback.term_dim_green;
    let term_dim_yellow = fallback.term_dim_yellow;
    let term_dim_blue = fallback.term_dim_blue;
    let term_dim_magenta = fallback.term_dim_magenta;
    let term_dim_cyan = fallback.term_dim_cyan;
    let term_dim_white = fallback.term_dim_white;

    let term_foreground =
        look(&["terminal.foreground", "foreground"]).unwrap_or(fallback.term_foreground);
    let term_bright_foreground = fallback.term_bright_foreground;
    let term_dim_foreground = fallback.term_dim_foreground;
    let term_background =
        look(&["terminal.background", "editor.background"]).unwrap_or(fallback.term_background);
    let term_background_unfocused = look(&["terminal.background", "editor.background"])
        .unwrap_or(fallback.term_background_unfocused);
    let cursor =
        look(&["terminalCursor.foreground", "editorCursor.foreground"]).unwrap_or(fallback.cursor);

    let scrollbar = look(&["scrollbarSlider.background"]).unwrap_or(fallback.scrollbar);
    let scrollbar_hover =
        look(&["scrollbarSlider.hoverBackground"]).unwrap_or(fallback.scrollbar_hover);

    let success = look(&[
        "gitDecoration.addedResourceForeground",
        "testing.iconPassed",
        "charts.green",
    ])
    .unwrap_or(fallback.success);
    let warning = look(&[
        "editorWarning.foreground",
        "list.warningForeground",
        "charts.yellow",
    ])
    .unwrap_or(fallback.warning);
    let error = look(&["errorForeground", "editorError.foreground", "charts.red"])
        .unwrap_or(fallback.error);

    let button_primary_bg = look(&["button.background"]).unwrap_or(fallback.button_primary_bg);
    let button_primary_fg = look(&["button.foreground"]).unwrap_or(fallback.button_primary_fg);
    let button_primary_hover =
        look(&["button.hoverBackground"]).unwrap_or(fallback.button_primary_hover);

    let folder_default = text_primary;
    let folder_red =
        look(&["terminal.ansiBrightRed", "terminal.ansiRed"]).unwrap_or(fallback.folder_red);
    let folder_orange =
        look(&["charts.orange", "terminal.ansiYellow"]).unwrap_or(fallback.folder_orange);
    let folder_yellow = look(&["terminal.ansiBrightYellow", "terminal.ansiYellow"])
        .unwrap_or(fallback.folder_yellow);
    let folder_lime =
        look(&["charts.lines", "terminal.ansiBrightGreen"]).unwrap_or(fallback.folder_lime);
    let folder_green =
        look(&["terminal.ansiBrightGreen", "terminal.ansiGreen"]).unwrap_or(fallback.folder_green);
    let folder_teal = look(&["terminal.ansiCyan"]).unwrap_or(fallback.folder_teal);
    let folder_cyan =
        look(&["terminal.ansiBrightCyan", "terminal.ansiCyan"]).unwrap_or(fallback.folder_cyan);
    let folder_blue =
        look(&["terminal.ansiBrightBlue", "terminal.ansiBlue"]).unwrap_or(fallback.folder_blue);
    let folder_indigo = look(&["terminal.ansiBlue"]).unwrap_or(fallback.folder_indigo);
    let folder_purple = look(&["terminal.ansiBrightMagenta", "terminal.ansiMagenta"])
        .unwrap_or(fallback.folder_purple);
    let folder_pink =
        look(&["terminal.ansiMagenta", "charts.purple"]).unwrap_or(fallback.folder_pink);

    let metric_normal = success;
    let metric_warning = warning;
    let metric_critical = error;

    // Diff backgrounds: take the raw RGB. The renderer applies its own
    // alpha (LINE_BG_ALPHA / WORD_BG_ALPHA) — pre-blending here would dim
    // the colour twice and the overlays disappear on dark themes.
    let diff_added_bg = look_raw(&[
        "diffEditor.insertedTextBackground",
        "diffEditor.insertedLineBackground",
    ])
    .unwrap_or(fallback.diff_added_bg);
    let diff_removed_bg = look_raw(&[
        "diffEditor.removedTextBackground",
        "diffEditor.removedLineBackground",
    ])
    .unwrap_or(fallback.diff_removed_bg);
    let diff_added_fg =
        look(&["gitDecoration.addedResourceForeground"]).unwrap_or(fallback.diff_added_fg);
    let diff_removed_fg =
        look(&["gitDecoration.deletedResourceForeground"]).unwrap_or(fallback.diff_removed_fg);
    let diff_hunk_header_bg = look(&["diffEditor.diagonalFill", "editorGutter.background"])
        .unwrap_or(fallback.diff_hunk_header_bg);
    let diff_hunk_header_fg =
        look(&["editorLineNumber.foreground"]).unwrap_or(fallback.diff_hunk_header_fg);

    ThemeColors {
        bg_primary,
        bg_secondary,
        bg_header,
        bg_selection,
        bg_hover,
        border,
        border_active,
        border_focused,
        border_bell,
        border_idle,
        text_primary,
        text_secondary,
        text_muted,
        selection_bg,
        selection_fg,
        search_match_bg,
        search_current_bg,
        term_black,
        term_red,
        term_green,
        term_yellow,
        term_blue,
        term_magenta,
        term_cyan,
        term_white,
        term_bright_black,
        term_bright_red,
        term_bright_green,
        term_bright_yellow,
        term_bright_blue,
        term_bright_magenta,
        term_bright_cyan,
        term_bright_white,
        term_dim_black,
        term_dim_red,
        term_dim_green,
        term_dim_yellow,
        term_dim_blue,
        term_dim_magenta,
        term_dim_cyan,
        term_dim_white,
        term_foreground,
        term_bright_foreground,
        term_dim_foreground,
        term_background,
        term_background_unfocused,
        cursor,
        scrollbar,
        scrollbar_hover,
        success,
        warning,
        error,
        button_primary_bg,
        button_primary_fg,
        button_primary_hover,
        folder_default,
        folder_red,
        folder_orange,
        folder_yellow,
        folder_lime,
        folder_green,
        folder_teal,
        folder_cyan,
        folder_blue,
        folder_indigo,
        folder_purple,
        folder_pink,
        metric_normal,
        metric_warning,
        metric_critical,
        diff_added_bg,
        diff_removed_bg,
        diff_added_fg,
        diff_removed_fg,
        diff_hunk_header_bg,
        diff_hunk_header_fg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vryn_core::theme::DARK_THEME;

    #[test]
    fn parses_6_digit_hex() {
        assert_eq!(parse_vscode_color("#1e1e1e"), Some(0x1e1e1e));
        assert_eq!(parse_vscode_color("1e1e1e"), Some(0x1e1e1e));
    }

    #[test]
    fn strips_alpha_from_8_digit_hex() {
        assert_eq!(parse_vscode_color("#1e1e1eff"), Some(0x1e1e1e));
        assert_eq!(parse_vscode_color("#1e1e1e80"), Some(0x1e1e1e));
    }

    #[test]
    fn rgba_reports_alpha_channel() {
        assert_eq!(parse_vscode_color_rgba("#1e1e1e"), Some((0x1e1e1e, 0xff)));
        assert_eq!(parse_vscode_color_rgba("#1e1e1e80"), Some((0x1e1e1e, 0x80)));
        assert_eq!(parse_vscode_color_rgba("#00000000"), Some((0x000000, 0x00)));
    }

    #[test]
    fn blend_over_handles_endpoints() {
        // Fully opaque: returns fg unchanged.
        assert_eq!(blend_over(0xff0000, 0xff, 0x000000), 0xff0000);
        // Fully transparent: returns bg unchanged.
        assert_eq!(blend_over(0xff0000, 0x00, 0x123456), 0x123456);
        // 50% white over black → mid grey.
        let mid = blend_over(0xffffff, 0x80, 0x000000);
        let r = (mid >> 16) & 0xff;
        assert!((0x7f..=0x81).contains(&r), "expected ~0x80, got {r:#x}");
    }

    #[test]
    fn mapping_blends_alpha_selection_against_bg() {
        // Poimandres uses `list.activeSelectionBackground: #30334080` (50% over bg).
        // Expected: a subtle dark shade near bg_primary (#1b1e28), NOT the raw #303340.
        let mut colors = HashMap::new();
        colors.insert("editor.background".into(), "#1b1e28".into());
        colors.insert("list.activeSelectionBackground".into(), "#30334080".into());
        let theme = VsCodeTheme {
            name: None,
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        // Raw #303340 would be much brighter; blended result should be closer to bg_primary.
        assert_ne!(mapped.bg_selection, 0x303340, "must blend, not strip alpha");
        let r = (mapped.bg_selection >> 16) & 0xff;
        assert!(
            r < 0x30,
            "blended red channel should be darker than raw: got {r:#x}"
        );
    }

    #[test]
    fn border_is_brighter_than_bg_for_dark_themes() {
        // Poimandres sets `panel.border: #00000030` → blended against dark bg
        // gives a line *darker* than bg (invisible). ensure_visible_border
        // must promote this to a subtle *lighter* line.
        let mut colors = HashMap::new();
        colors.insert("editor.background".into(), "#1b1e28".into());
        colors.insert("foreground".into(), "#a6accd".into());
        colors.insert("panel.border".into(), "#00000030".into());
        let theme = VsCodeTheme {
            name: None,
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert!(
            luma(mapped.border) > luma(0x1b1e28),
            "dark-theme border must be lighter than bg, got {:#08x} vs bg {:#08x}",
            mapped.border,
            0x1b1e28
        );
    }

    #[test]
    fn border_kept_when_already_visible() {
        // If the theme supplies an opaque visible border, don't override it.
        let mut colors = HashMap::new();
        colors.insert("editor.background".into(), "#1e1e1e".into());
        colors.insert("foreground".into(), "#d4d4d4".into());
        colors.insert("panel.border".into(), "#464b57ff".into());
        let theme = VsCodeTheme {
            name: None,
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.border, 0x464b57);
    }

    #[test]
    fn mapping_skips_fully_transparent_keys() {
        // Poimandres sets `focusBorder: #00000000`. My border_focused chain
        // should skip this (alpha = 0) and fall through to a visible fallback.
        let mut colors = HashMap::new();
        colors.insert("editor.background".into(), "#1b1e28".into());
        colors.insert("focusBorder".into(), "#00000000".into());
        colors.insert("activityBar.activeBorder".into(), "#a6accd".into());
        let theme = VsCodeTheme {
            name: None,
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.border_focused, 0xa6accd);
    }

    #[test]
    fn expands_3_digit_shorthand() {
        assert_eq!(parse_vscode_color("#f0a"), Some(0xff00aa));
        assert_eq!(parse_vscode_color("#abc"), Some(0xaabbcc));
    }

    #[test]
    fn rejects_invalid_hex() {
        assert_eq!(parse_vscode_color("not-a-color"), None);
        assert_eq!(parse_vscode_color("#xyz"), None);
        assert_eq!(parse_vscode_color(""), None);
    }

    #[test]
    fn detects_vscode_schema_by_dot_keys() {
        let v: serde_json::Value =
            serde_json::from_str(r##"{ "colors": { "editor.background": "#000" } }"##).unwrap();
        assert!(is_vscode_schema(&v));
    }

    #[test]
    fn rejects_vryn_native_schema() {
        let v: serde_json::Value =
            serde_json::from_str(r##"{ "colors": { "bg_primary": "#000" } }"##).unwrap();
        assert!(!is_vscode_schema(&v));
    }

    #[test]
    fn detects_vscode_via_include() {
        let v: serde_json::Value =
            serde_json::from_str(r##"{ "include": "./other.json" }"##).unwrap();
        assert!(is_vscode_schema(&v));
    }

    #[test]
    fn strips_line_comments() {
        let input = "{\n  // trailing comment\n  \"a\": 1\n}";
        let out = strip_jsonc(input);
        assert!(!out.contains("trailing comment"));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn strips_block_comments() {
        let input = "{ /* block\n comment */ \"a\": 1 }";
        let out = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn preserves_slashes_in_strings() {
        let input = r#"{ "url": "http://example.com" }"#;
        let out = strip_jsonc(input);
        assert!(out.contains("http://example.com"));
    }

    #[test]
    fn strips_trailing_commas_in_objects_and_arrays() {
        let input = r#"{ "a": 1, "b": [1, 2, 3,], }"#;
        let out = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"][2], 3);
    }

    #[test]
    fn preserves_commas_inside_strings() {
        let input = r#"{ "msg": "one, two," }"#;
        let out = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["msg"], "one, two,");
    }

    #[test]
    fn maps_known_vscode_keys() {
        let mut colors = HashMap::new();
        colors.insert("editor.background".to_string(), "#111111".to_string());
        colors.insert("foreground".to_string(), "#eeeeee".to_string());
        colors.insert("terminal.ansiRed".to_string(), "#ff0000".to_string());
        let theme = VsCodeTheme {
            name: Some("T".into()),
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.bg_primary, 0x111111);
        assert_eq!(mapped.text_primary, 0xeeeeee);
        assert_eq!(mapped.term_red, 0xff0000);
    }

    #[test]
    fn maps_sidebar_background_to_secondary_surface() {
        let mut colors = HashMap::new();
        colors.insert("editor.background".to_string(), "#282c34".to_string());
        colors.insert("sideBar.background".to_string(), "#21252b".to_string());
        colors.insert(
            "sideBarSectionHeader.background".to_string(),
            "#282c34".to_string(),
        );
        let theme = VsCodeTheme {
            name: Some("One Dark Pro".into()),
            kind: Some("dark".into()),
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.bg_primary, 0x282c34);
        assert_eq!(mapped.bg_secondary, 0x21252b);
    }

    #[test]
    fn falls_back_through_key_chain() {
        // bg_primary chain: editor.background, sideBar.background, panel.background
        let mut colors = HashMap::new();
        colors.insert("sideBar.background".to_string(), "#222222".to_string());
        let theme = VsCodeTheme {
            name: None,
            kind: None,
            include: None,
            colors,
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.bg_primary, 0x222222);
    }

    #[test]
    fn uses_fallback_for_missing_keys() {
        let theme = VsCodeTheme {
            name: None,
            kind: None,
            include: None,
            colors: HashMap::new(),
        };
        let mapped = vscode_to_theme_colors(&theme, &DARK_THEME);
        assert_eq!(mapped.bg_primary, DARK_THEME.bg_primary);
        assert_eq!(mapped.term_red, DARK_THEME.term_red);
    }

    #[test]
    fn is_dark_from_type_field() {
        let dark = VsCodeTheme {
            name: None,
            kind: Some("dark".into()),
            include: None,
            colors: HashMap::new(),
        };
        assert!(dark.is_dark());
        let light = VsCodeTheme {
            name: None,
            kind: Some("light".into()),
            include: None,
            colors: HashMap::new(),
        };
        assert!(!light.is_dark());
    }

    #[test]
    fn is_dark_infers_from_background_when_type_missing() {
        let mut light_colors = HashMap::new();
        light_colors.insert("editor.background".into(), "#fdf6e3".into());
        let light = VsCodeTheme {
            name: None,
            kind: None,
            include: None,
            colors: light_colors,
        };
        assert!(!light.is_dark());

        let mut dark_colors = HashMap::new();
        dark_colors.insert("editor.background".into(), "#1e1e1e".into());
        let dark = VsCodeTheme {
            name: None,
            kind: None,
            include: None,
            colors: dark_colors,
        };
        assert!(dark.is_dark());
    }

    #[test]
    fn include_chain_merges_with_child_precedence() {
        let tmp = std::env::temp_dir().join(format!("vryn-vscode-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let parent = tmp.join("parent.json");
        let child = tmp.join("child.json");
        std::fs::write(
            &parent,
            r##"{ "name": "Parent", "type": "dark", "colors": { "editor.background": "#111111", "foreground": "#cccccc" } }"##,
        )
        .unwrap();
        std::fs::write(
            &child,
            r##"{ "include": "./parent.json", "colors": { "editor.background": "#222222" } }"##,
        )
        .unwrap();
        let merged = load_with_includes(&child).unwrap();
        assert_eq!(
            merged.colors.get("editor.background").map(String::as_str),
            Some("#222222")
        );
        assert_eq!(
            merged.colors.get("foreground").map(String::as_str),
            Some("#cccccc")
        );
        assert_eq!(merged.name.as_deref(), Some("Parent"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn include_loop_guard_triggers() {
        let tmp = std::env::temp_dir().join(format!("vryn-vscode-loop-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("a.json");
        let b = tmp.join("b.json");
        std::fs::write(&a, r##"{ "include": "./b.json", "colors": {} }"##).unwrap();
        std::fs::write(&b, r##"{ "include": "./a.json", "colors": {} }"##).unwrap();
        let res = load_with_includes(&a);
        assert!(res.is_err(), "expected include loop to fail");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
