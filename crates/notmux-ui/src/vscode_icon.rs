//! File-type icon backed by the official `vscode-icons` SVG set.
//!
//! GPUI's `svg()` element renders SVGs as a single tint color (it ignores
//! per-path fills). We embed the original vscode-icons SVGs (MIT) and tint
//! each one with the language's signature color. Result: real vscode-icon
//! shape, language-coloured silhouette.
//!
//! License: vscode-icons is MIT-licensed.
//! https://github.com/vscode-icons/vscode-icons

use crate::theme::ThemeColors;
use gpui::*;

/// Render a 16×16 file-type icon. Convenience wrapper over [`vscode_file_icon_sized`].
pub fn vscode_file_icon(filename: &str, t: &ThemeColors, cx: &App) -> Div {
    vscode_file_icon_sized(filename, px(16.0), t, cx)
}

/// Render a 16×16 file-type icon, optionally using a monochrome theme color.
pub fn vscode_file_icon_with_options(
    filename: &str,
    t: &ThemeColors,
    monochrome: bool,
    cx: &App,
) -> Div {
    vscode_file_icon_sized_with_options(filename, px(16.0), t, monochrome, cx)
}

/// Render a file-type icon at the given size.
///
/// The returned div is `size × size` and `flex_shrink_0` so it sits cleanly
/// in flex rows. Unknown extensions fall back to `default.svg` tinted with
/// the theme's muted text color.
pub fn vscode_file_icon_sized(filename: &str, size: Pixels, t: &ThemeColors, _cx: &App) -> Div {
    vscode_file_icon_sized_with_options(filename, size, t, false, _cx)
}

/// Render a file-type icon at the given size, optionally using a monochrome theme color.
pub fn vscode_file_icon_sized_with_options(
    filename: &str,
    size: Pixels,
    t: &ThemeColors,
    monochrome: bool,
    _cx: &App,
) -> Div {
    let (icon, color) = icon_for(filename, t);
    let color = if monochrome { t.text_primary } else { color };
    div()
        .size(size)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(format!("icons/file-types/{}.svg", icon))
                .size(size)
                .text_color(rgb(color)),
        )
}

/// Map a filename to (icon stem, tint color).
///
/// First checks special filenames (`Cargo.toml`, `package.json`, `Dockerfile`,
/// `.gitignore`, `LICENSE`, `README*`), then falls back to the file extension.
fn icon_for(filename: &str, t: &ThemeColors) -> (&'static str, u32) {
    // Special filenames — match case-insensitively against the basename.
    let lower = filename.to_ascii_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(&lower);

    if basename == "dockerfile" || basename.ends_with(".dockerfile") {
        return ("docker", 0x0db7ed);
    }
    if basename == "package.json" || basename == "package-lock.json" {
        return ("package", 0xcb3837);
    }
    if basename == "cargo.toml" || basename == "cargo.lock" {
        return ("rust", 0xdea584);
    }
    if basename == ".gitignore"
        || basename == ".gitattributes"
        || basename == ".gitmodules"
        || basename == ".gitconfig"
    {
        return ("git", 0xf05133);
    }
    if basename == "license"
        || basename == "licence"
        || basename.starts_with("license.")
        || basename.starts_with("licence.")
    {
        return ("license", 0xcba63c);
    }
    if basename == "readme" || basename.starts_with("readme.") {
        return ("markdown", 0x569cd6);
    }

    let ext = basename.rsplit('.').next().unwrap_or("");
    match ext {
        // Rust
        "rs" => ("rust", 0xdea584),
        // Python
        "py" | "pyw" | "pyi" => ("python", 0x3776ab),
        // TypeScript / React
        "ts" | "mts" | "cts" => ("typescript", 0x3178c6),
        "tsx" => ("reactts", 0x3178c6),
        "jsx" => ("reactjs", 0x61dafb),
        // JavaScript
        "js" | "mjs" | "cjs" => ("js", 0xf7df1e),
        // Web
        "html" | "htm" => ("html", 0xe34c26),
        "css" => ("css", 0x563d7c),
        "scss" => ("scss", 0xcd6799),
        "sass" => ("sass", 0xcd6799),
        "less" => ("less", 0x1d365d),
        // Data
        "json" | "jsonc" => ("json", 0xcba63c),
        "yaml" | "yml" => ("yaml", 0xc678dd),
        "toml" => ("toml", 0x9c4221),
        // Markdown
        "md" | "mdx" | "markdown" => ("markdown", 0x569cd6),
        // Go
        "go" => ("go", 0x00add8),
        // JVM
        "java" => ("java", 0xe76f00),
        "kt" | "kts" => ("kotlin", 0x7f52ff),
        // C family
        "c" => ("c", 0xa8b9cc),
        "cpp" | "cc" | "cxx" => ("cpp", 0x00599c),
        "h" | "hpp" | "hxx" => ("cheader", 0x6295cb),
        "cs" => ("csharp", 0x9b4dca),
        // Ruby / PHP
        "rb" | "erb" | "rake" => ("ruby", 0xcc342d),
        "php" => ("php", 0x787cb5),
        // Mobile
        "swift" => ("swift", 0xfa7343),
        "dart" => ("dart", 0x0175c2),
        // Shell
        "sh" | "bash" | "zsh" | "fish" => ("shell", 0x4eaa25),
        "ps1" | "psm1" | "psd1" => ("powershell", 0x5391fe),
        // Misc
        "sql" => ("sql", 0xe38c00),
        "xml" => ("xml", 0xe44d26),
        "svg" => ("svg", 0xffb13b),
        "lua" => ("lua", 0x000080),
        "zig" => ("zig", 0xf7a41d),
        "r" | "rmd" => ("r", 0x276dc3),
        "ex" | "exs" => ("elixir", 0x6e4a7e),
        "erl" | "hrl" => ("erlang", 0xa90533),
        "nix" => ("nix", 0x7e7eff),
        "hs" | "lhs" => ("haskell", 0x5e5086),
        "vue" => ("vue", 0x4fc08d),
        "svelte" => ("svelte", 0xff3e00),
        // Images
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "tiff" => ("image", 0xa074c4),
        // Binary
        "exe" | "dll" | "so" | "dylib" | "bin" | "wasm" => ("binary", 0xa3a3a3),
        // Plain text
        "txt" | "log" | "text" => ("text", t.text_muted),
        // Unknown — generic file
        _ => ("default", t.text_muted),
    }
}
