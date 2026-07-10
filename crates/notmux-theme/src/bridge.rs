//! Bridges the app's `GpuiTheme` (the notagent theme system) into the
//! `ThemeColors` palette the notmux views read.
//!
//! Both functions are exact ports of the bridges the notagent GUI uses to
//! theme these same views when it embeds them (`terminal_panel.rs` /
//! `git_panel.rs` in `notagent_gpui`): a light app theme yields the light
//! palette, otherwise the dark one; a set of chrome colors is then overridden
//! to follow the app theme exactly, and the remaining fields keep the palette
//! defaults.

use gpui::App;
use notagent_theme::ThemeColor;
use notmux_core::theme::{DARK_THEME, LIGHT_THEME, ThemeColors};

use crate::gpui_theme::GpuiTheme;

/// The terminal color theme, following the app theme: a light app theme yields
/// the light terminal palette, otherwise the dark one. Read by the terminal view
/// via the global theme provider, so the terminal re-colors with the app.
pub fn terminal_theme(cx: &App) -> ThemeColors {
    let g = cx.global::<GpuiTheme>();
    let bg = g.bg_base();
    let mut theme = if bg.l > 0.5 { LIGHT_THEME } else { DARK_THEME };
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let to_u32 = |h: gpui::Hsla| {
        let r = gpui::Rgba::from(h);
        (to_u8(r.r) << 16) | (to_u8(r.g) << 8) | to_u8(r.b)
    };
    let bg_u32 = to_u32(bg);
    theme.bg_primary = bg_u32;
    theme.bg_header = bg_u32;
    // The terminal grid paints its background from term_background; match it too.
    theme.term_background = bg_u32;
    theme.term_background_unfocused = bg_u32;
    // Tab chrome follows the app theme so the terminal tabs match the browser
    // tabs: active tab = surface_1, hover = surface_2, neutral text/border.
    theme.bg_secondary = to_u32(g.surface_1());
    theme.bg_hover = to_u32(g.surface_2());
    theme.border = to_u32(g.outline());
    theme.text_primary = to_u32(g.fg(ThemeColor::Text));
    theme.text_secondary = to_u32(g.fg(ThemeColor::Muted));
    theme.text_muted = to_u32(g.fg(ThemeColor::Muted));
    theme
}

/// The sidebar (left panel) color theme. Matches the notagent left panel:
/// the panel surface is `surface_2` (rendered at 90% alpha over the system
/// blur by the root chrome in transparent mode), rows hover/select with
/// `surface_3`.
pub fn sidebar_theme(cx: &App) -> ThemeColors {
    let g = cx.global::<GpuiTheme>();
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let to_u32 = |h: gpui::Hsla| {
        let r = gpui::Rgba::from(h);
        (to_u8(r.r) << 16) | (to_u8(r.g) << 8) | to_u8(r.b)
    };
    let mut theme = terminal_theme(cx);
    // Panel surface: the reference chrome (surface_2).
    theme.bg_primary = to_u32(g.surface_2());
    theme.bg_header = to_u32(g.surface_2());
    // Row hover/selection sit one elevation step above the panel (surface_3),
    // exactly like the notagent left panel's hover_bg/sel_bg.
    theme.bg_hover = to_u32(g.surface_3());
    theme.bg_secondary = to_u32(g.surface_3());
    theme
}

/// Bridges the app theme into the `ThemeColors` palette the git view reads.
/// A light app theme yields the light git palette, otherwise the dark one; a
/// handful of chrome colors are then overridden to follow the app exactly (the
/// rest — diff added/removed colors etc. — keep the palette defaults).
pub fn git_theme(cx: &App) -> ThemeColors {
    let g = cx.global::<GpuiTheme>();
    let bg = g.bg_base();
    let mut theme = if bg.l > 0.5 { LIGHT_THEME } else { DARK_THEME };
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let to_u32 = |h: gpui::Hsla| {
        let r = gpui::Rgba::from(h);
        (to_u8(r.r) << 16) | (to_u8(r.g) << 8) | to_u8(r.b)
    };
    theme.bg_primary = to_u32(bg);
    theme.bg_secondary = to_u32(g.bg_base());
    theme.bg_header = to_u32(g.surface_1());
    theme.bg_hover = to_u32(g.surface_2());
    theme.bg_selection = to_u32(g.selection());
    theme.selection_bg = to_u32(g.selection());
    // The file viewer derives its header chrome by blending over
    // `term_background` — keep it on the app background.
    theme.term_background = to_u32(bg);
    theme.term_background_unfocused = to_u32(bg);
    theme.border = to_u32(g.outline());
    // Scrollbars must look identical to the app's (track = surface_1 via
    // bg_header, thumb = surface_3, hover = muted — the exact palette
    // the notagent scrollbar uses).
    theme.scrollbar = to_u32(g.surface_3());
    theme.scrollbar_hover = to_u32(g.fg(ThemeColor::Muted));
    theme.text_primary = to_u32(g.fg(ThemeColor::Text));
    theme.text_secondary = to_u32(g.fg(ThemeColor::Muted));
    theme.text_muted = to_u32(g.fg(ThemeColor::Muted));
    // Status colors (added/modified/deleted decorations in the file explorer
    // and git views) follow the app theme.
    theme.success = to_u32(g.fg(ThemeColor::Success));
    theme.warning = to_u32(g.fg(ThemeColor::Warning));
    theme.error = to_u32(g.fg(ThemeColor::Error));
    theme.term_green = to_u32(g.fg(ThemeColor::Success));
    theme.term_yellow = to_u32(g.fg(ThemeColor::Warning));
    theme.term_red = to_u32(g.fg(ThemeColor::Error));
    // Syntax highlighting palette (file viewer + diff views) follows the app
    // theme, so code in the editor matches code rendered elsewhere.
    theme.syntax_comment = to_u32(g.fg(ThemeColor::SyntaxComment));
    theme.syntax_string = to_u32(g.fg(ThemeColor::SyntaxString));
    theme.syntax_keyword = to_u32(g.fg(ThemeColor::SyntaxKeyword));
    theme.syntax_number = to_u32(g.fg(ThemeColor::SyntaxNumber));
    theme.syntax_type = to_u32(g.fg(ThemeColor::SyntaxType));
    theme.syntax_function = to_u32(g.fg(ThemeColor::SyntaxFunction));
    theme.syntax_variable = to_u32(g.fg(ThemeColor::SyntaxVariable));
    // No dedicated property color in the app theme; variables read closest.
    theme.syntax_property = to_u32(g.fg(ThemeColor::SyntaxVariable));
    theme.syntax_operator = to_u32(g.fg(ThemeColor::SyntaxOperator));
    theme.syntax_punctuation = to_u32(g.fg(ThemeColor::SyntaxPunctuation));
    theme
}
