//! Key routing between NotMux and the program inside the focused terminal.
//!
//! Mirrors how the reference implementation decides this (`Sources/AppDelegate.swift:17483` in
//! `the reference implementation`): while a terminal owns the focus, the app only claims
//! keystrokes that carry the platform's app modifier — every other keystroke
//! goes straight to the terminal. There is no detection of which keys the
//! running agent wants; the terminal simply has first claim, so an agent's
//! Ctrl-chords, Esc and bare keys reach it by default.
//!
//! On macOS the app modifier is Cmd, exactly as in the reference implementation Linux and Windows have
//! no free equivalent — Ctrl belongs to the shell there — so NotMux follows the
//! terminal-emulator convention on those platforms and treats Ctrl+Shift (and
//! Ctrl+Alt) as the app modifier, the way GNOME Terminal, Konsole and Windows
//! Terminal do.

/// Where a configured binding is allowed to fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// Register unchanged — the app owns this keystroke everywhere.
    App,
    /// Register scoped to `!TerminalPane` — the app owns the keystroke only
    /// while no terminal has focus.
    AppOutsideTerminal,
    /// Do not register at all — the keystroke belongs to the terminal.
    Terminal,
}

/// Keystrokes the app keeps even without the app modifier, because a terminal
/// cannot make use of them anyway. This is the equivalent of the reference implementation's narrow
/// exception list in `the reference implementation`.
const APP_OWNED_KEYSTROKES: &[&str] = &[
    // No legacy terminal encoding exists for these.
    "ctrl-tab",
    "ctrl-shift-tab",
    // Scrollback is the emulator's job, not the shell's.
    "shift-pageup",
    "shift-pagedown",
    // NotMux fullscreen; TUIs use plain Esc, not Shift+Esc.
    "shift-escape",
];

/// The key context of a focused terminal pane.
pub const TERMINAL_CONTEXT: &str = "TerminalPane";

/// Context predicate for "the app owns this, but only outside a terminal".
pub const OUTSIDE_TERMINAL_CONTEXT: &str = "!TerminalPane";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Mods {
    control: bool,
    shift: bool,
    alt: bool,
    platform: bool,
}

/// Parse the leading modifiers of a single chord such as `cmd-shift-d`.
///
/// Stops at the first token that is not a modifier, which is the key itself —
/// including keys that are a dash (`ctrl--`).
fn modifiers_of(chord: &str) -> Mods {
    let mut mods = Mods::default();
    let mut rest = chord;

    while let Some(idx) = rest.find('-') {
        let (head, tail) = rest.split_at(idx);
        match head {
            "cmd" | "super" | "win" => mods.platform = true,
            "ctrl" => mods.control = true,
            "alt" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            // The remainder is the key, not a modifier.
            _ => break,
        }
        rest = &tail[1..];
    }

    mods
}

/// Does this keystroke carry the modifier that makes it an app shortcut?
fn has_app_modifier(mods: Mods) -> bool {
    if cfg!(target_os = "macos") {
        mods.platform
    } else {
        mods.platform || (mods.control && (mods.shift || mods.alt))
    }
}

/// Decide where a configured binding may fire.
///
/// `keystroke` is the configured keystroke, possibly a chord such as
/// `cmd-k cmd-s`; only the opening chord decides, because that is the one the
/// terminal would lose while the matcher waits for the second stroke.
pub fn route(keystroke: &str, context: Option<&str>) -> Routing {
    // Bindings scoped to some other context (dialogs, the sidebar, the search
    // bar, the zoomed-pane mode) already say where they belong.
    match context {
        Some(ctx) if ctx != TERMINAL_CONTEXT => return Routing::App,
        _ => {}
    }

    let opener = keystroke.split_whitespace().next().unwrap_or(keystroke);

    if APP_OWNED_KEYSTROKES.contains(&opener) || has_app_modifier(modifiers_of(opener)) {
        return Routing::App;
    }

    // Without the app modifier the terminal wins wherever it has focus. A
    // binding that was scoped to the terminal has nowhere left to fire.
    match context {
        Some(_) => Routing::Terminal,
        None => Routing::AppOutsideTerminal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_including_dash_as_key() {
        assert_eq!(
            modifiers_of("ctrl--"),
            Mods {
                control: true,
                ..Default::default()
            }
        );
        assert_eq!(
            modifiers_of("cmd-shift-d"),
            Mods {
                platform: true,
                shift: true,
                ..Default::default()
            }
        );
        assert_eq!(
            modifiers_of("ctrl-alt-="),
            Mods {
                control: true,
                alt: true,
                ..Default::default()
            }
        );
        assert_eq!(modifiers_of("shift-escape"), Mods { shift: true, ..Default::default() });
    }

    #[test]
    fn platform_shortcuts_stay_app_owned() {
        assert_eq!(route("cmd-d", Some(TERMINAL_CONTEXT)), Routing::App);
        assert_eq!(route("cmd-k cmd-s", None), Routing::App);
        assert_eq!(route("cmd-alt-left", None), Routing::App);
    }

    #[test]
    fn terminal_keeps_control_chords_it_needs() {
        // Ctrl+[ is Esc, Ctrl+D is EOF, Ctrl+F/E/P/B are readline motions.
        for ks in ["ctrl-[", "ctrl-d", "ctrl-f", "ctrl-0"] {
            assert_eq!(
                route(ks, Some(TERMINAL_CONTEXT)),
                Routing::Terminal,
                "{ks} must reach the terminal"
            );
        }
        for ks in ["ctrl-e", "ctrl-p", "ctrl-b", "ctrl-`"] {
            assert_eq!(
                route(ks, None),
                Routing::AppOutsideTerminal,
                "{ks} must not fire while a terminal has focus"
            );
        }
    }

    #[test]
    fn chord_is_judged_by_its_opening_stroke() {
        // Ctrl+K is kill-line: the matcher must not swallow it while waiting.
        assert_eq!(route("ctrl-k ctrl-s", None), Routing::AppOutsideTerminal);
    }

    #[test]
    fn narrow_exceptions_stay_app_owned() {
        for ks in APP_OWNED_KEYSTROKES {
            assert_eq!(route(ks, Some(TERMINAL_CONTEXT)), Routing::App, "{ks}");
        }
    }

    #[test]
    fn other_contexts_are_left_alone() {
        assert_eq!(route("escape", Some("SearchBar")), Routing::App);
        assert_eq!(route("left", Some("TerminalPaneFullscreen")), Routing::App);
        assert_eq!(route("up", Some("Sidebar")), Routing::App);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ctrl_shift_is_the_app_modifier_off_macos() {
        assert_eq!(route("ctrl-shift-d", Some(TERMINAL_CONTEXT)), Routing::App);
        assert_eq!(route("ctrl-alt-=", None), Routing::App);
        assert_eq!(route("ctrl-d", Some(TERMINAL_CONTEXT)), Routing::Terminal);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_belongs_to_the_terminal_on_macos() {
        // Ctrl+Shift+D has no app meaning on macOS; Cmd+Shift+D is the binding.
        assert_eq!(route("ctrl-shift-d", Some(TERMINAL_CONTEXT)), Routing::Terminal);
    }
}
