#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod macros;
mod action_dispatch;
mod app;
mod assets;
mod cli;
mod elements;
mod git;
mod keybindings;
mod native_notify;
mod process;
mod remote;
mod remote_client;
mod services;
mod settings;
#[cfg(target_os = "linux")]
mod simple_root;
#[cfg(test)]
mod smoke_tests;
mod terminal;
mod theme;
mod ui;
mod views;
mod workspace;

#[cfg(target_os = "linux")]
use crate::simple_root::SimpleRoot as Root;
use gpui::*;
#[cfg(not(target_os = "linux"))]
use gpui_component::Root;
use gpui_component::theme::{Theme as GpuiComponentTheme, ThemeMode as GpuiThemeMode};
use std::sync::Arc;

use std::net::IpAddr;

/// Writes to both stderr and a log file simultaneously.
struct TeeWriter {
    stderr: std::io::Stderr,
    file: std::fs::File,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.stderr.write_all(buf);
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.stderr.flush();
        self.file.flush()
    }
}

use crate::app::NotMux;
use crate::app::headless::HeadlessApp;
use crate::assets::{Assets, embedded_fonts};
use crate::keybindings::{
    About, Quit, ShowCommandPalette, ShowKeybindings, ShowSettings, ShowThemeSelector,
};
use crate::settings::GlobalSettings;
use crate::terminal::pty_manager::PtyManager;
use crate::theme::GpuiTheme;
use crate::views::panels::toast::{Toast, ToastManager};
use crate::workspace::persistence;
use crate::workspace::state::GlobalWorkspace;

/// Quit action handler - flushes pending saves before exiting
fn quit(_: &Quit, cx: &mut App) {
    // Flush pending settings save
    if let Some(gs) = cx.try_global::<GlobalSettings>() {
        gs.0.read(cx).flush_pending_save();
    }

    // Flush pending workspace save
    if let Some(gw) = cx.try_global::<GlobalWorkspace>() {
        let active_session = cx
            .try_global::<GlobalSettings>()
            .and_then(|gs| gs.0.read(cx).settings.active_session.clone());
        if let Err(e) =
            persistence::save_active_workspace(gw.0.read(cx).data(), active_session.as_deref())
        {
            log::error!("Failed to flush workspace on quit: {}", e);
        }
    }

    cx.quit();
}

/// About action handler - shows native macOS about panel
#[cfg(target_os = "macos")]
fn about(_: &About, _cx: &mut App) {
    use std::ffi::{c_char, c_void};

    // Non-variadic objc_msgSend trampolines — ARM64 requires the standard
    // (non-variadic) calling convention; declaring `...` misplaces arguments.
    #[allow(clashing_extern_declarations)]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg(obj: *mut c_void, sel: *mut c_void) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_str(obj: *mut c_void, sel: *mut c_void, s: *const u8) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_id(obj: *mut c_void, sel: *mut c_void, a: *mut c_void) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_id2(
            obj: *mut c_void,
            sel: *mut c_void,
            a: *mut c_void,
            b: *mut c_void,
        ) -> *mut c_void;

        #[link_name = "objc_msgSend"]
        fn msg_bytes_len(
            obj: *mut c_void,
            sel: *mut c_void,
            bytes: *const u8,
            len: usize,
        ) -> *mut c_void;
    }

    unsafe {
        let alloc = sel_registerName(c"alloc".as_ptr());
        let init_utf8 = sel_registerName(c"initWithUTF8String:".as_ptr());
        let ns_string = objc_getClass(c"NSString".as_ptr());

        // Helper: create NSString from null-terminated bytes
        let nsstring =
            |s: &[u8]| -> *mut c_void { msg_str(msg(ns_string, alloc), init_utf8, s.as_ptr()) };

        // Build options dictionary with version
        let dict = msg(
            objc_getClass(c"NSMutableDictionary".as_ptr()),
            sel_registerName(c"new".as_ptr()),
        );
        let set_obj = sel_registerName(c"setObject:forKey:".as_ptr());
        let version_cstr = concat!(env!("CARGO_PKG_VERSION"), "\0");
        msg_id2(
            dict,
            set_obj,
            nsstring(version_cstr.as_bytes()),
            nsstring(b"ApplicationVersion\0"),
        );
        // Set build number to empty to hide the "(x.y.z)" parenthetical
        msg_id2(dict, set_obj, nsstring(b"\0"), nsstring(b"Version\0"));
        // Override copyright from Info.plist to ensure it's always current
        msg_id2(
            dict,
            set_obj,
            nsstring(b"Copyright \xC2\xA9 2026 Contember. All rights reserved.\0"),
            nsstring(b"Copyright\0"),
        );

        // Load embedded app icon as NSImage
        let icon_png = include_bytes!("../assets/logo.png");
        let ns_data = msg_bytes_len(
            objc_getClass(c"NSData".as_ptr()),
            sel_registerName(c"dataWithBytes:length:".as_ptr()),
            icon_png.as_ptr(),
            icon_png.len(),
        );
        let ns_image = msg_id(
            msg(objc_getClass(c"NSImage".as_ptr()), alloc),
            sel_registerName(c"initWithData:".as_ptr()),
            ns_data,
        );
        if !ns_image.is_null() {
            msg_id2(dict, set_obj, ns_image, nsstring(b"ApplicationIcon\0"));
        }

        // Credits as attributed string from HTML (supports clickable link)
        let html = b"<div style=\"text-align:center; font-family:-apple-system; font-size:11px;\">Created by Contember Ltd.<br><a href=\"https://contember.com\">contember.com</a></div>";
        let html_data = msg_bytes_len(
            objc_getClass(c"NSData".as_ptr()),
            sel_registerName(c"dataWithBytes:length:".as_ptr()),
            html.as_ptr(),
            html.len(),
        );
        let credits = msg_id2(
            msg(objc_getClass(c"NSAttributedString".as_ptr()), alloc),
            sel_registerName(c"initWithHTML:documentAttributes:".as_ptr()),
            html_data,
            std::ptr::null_mut::<c_void>(),
        );
        if !credits.is_null() {
            msg_id2(dict, set_obj, credits, nsstring(b"Credits\0"));
        }

        // [[NSApplication sharedApplication] orderFrontStandardAboutPanelWithOptions:dict]
        let app = msg(
            objc_getClass(c"NSApplication".as_ptr()),
            sel_registerName(c"sharedApplication".as_ptr()),
        );
        msg_id(
            app,
            sel_registerName(c"orderFrontStandardAboutPanelWithOptions:".as_ptr()),
            dict,
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn about(_: &About, _cx: &mut App) {
    log::info!("NotMux v{}", env!("CARGO_PKG_VERSION"));
}

/// Set up macOS application menu
fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu {
            name: "NotMux".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About NotMux", About),
                MenuItem::separator(),
                MenuItem::action("Settings...", ShowSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit NotMux", Quit),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", crate::keybindings::Copy, OsAction::Undo), // Using Copy as placeholder since we need an action
                MenuItem::os_action("Redo", crate::keybindings::Copy, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", crate::keybindings::Copy, OsAction::Cut),
                MenuItem::os_action("Copy", crate::keybindings::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", crate::keybindings::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", crate::keybindings::Copy, OsAction::SelectAll),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Command Palette", ShowCommandPalette),
                MenuItem::action("Select Theme", ShowThemeSelector),
                MenuItem::separator(),
                MenuItem::action("Keyboard Shortcuts", ShowKeybindings),
            ],
        },
    ]);
}

/// `notmux pair` — generate a pairing code and write it to a file for the running server to validate.
/// Global handle keeping the headless app entity alive for the process lifetime.
struct GlobalHeadless(#[allow(dead_code)] Entity<HeadlessApp>);
impl Global for GlobalHeadless {}

/// Run the application in headless mode (no GUI, remote server only).
fn run_headless(listen_addr: IpAddr) {
    println!("Starting NotMux in headless mode...");

    Application::with_platform(gpui_platform::current_platform(true)).run(move |cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);

        // Initialize global settings (must be before workspace load)
        let settings_entity = settings::init_settings(cx);
        let mut app_settings = settings_entity.read(cx).get().clone();
        if let Some(active_session) = app_settings.active_session.clone()
            && !persistence::session_exists(&active_session)
        {
            settings_entity.update(cx, |state, cx| {
                state.settings.active_session = None;
                state.save_and_notify(cx);
            });
            app_settings.active_session = None;
        }

        // Load or create workspace
        let workspace_data = persistence::load_active_workspace(&app_settings).unwrap_or_else(|e| {
            log::error!("Failed to load workspace: {}. A backup may have been saved to {:?}. Using default workspace.", e, persistence::get_workspace_path().with_extension("json.bak"));
            settings_entity.update(cx, |state, cx| {
                state.settings.active_session = None;
                state.save_and_notify(cx);
            });
            persistence::default_workspace()
        });

        // Create PTY manager
        let (pty_manager, pty_events) = PtyManager::new(app_settings.session_backend);
        let pty_manager = Arc::new(pty_manager);

        // Create the headless app entity (starts PTY loop, command loop, and remote server)
        // Must be stored in a global to keep the entity alive — dropping the handle
        // would release the entity and cancel all spawned tasks + drop RemoteServer.
        let headless = cx.new(|cx| {
            HeadlessApp::new(workspace_data, pty_manager, pty_events, listen_addr, cx)
        });
        cx.set_global(GlobalHeadless(headless));
    });
}

fn main() {
    // Handle --version before initializing anything (used by updater validation)
    if std::env::args().any(|a| a == "--version") {
        println!("notmux {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Handle CLI subcommands before GPUI init
    if let Some(exit_code) = cli::try_handle_cli() {
        std::process::exit(exit_code);
    }

    // Set up file logging: rotate previous log, write to both stderr and file
    let log_target = (|| -> Option<env_logger::fmt::Target> {
        let config_dir = persistence::get_config_dir();
        std::fs::create_dir_all(&config_dir).ok()?;
        let log_path = config_dir.join("notmux.log");
        let prev_path = config_dir.join("notmux.log.1");
        if log_path.exists() {
            let _ = std::fs::rename(&log_path, &prev_path);
        }
        let file = std::fs::File::create(&log_path).ok()?;
        Some(env_logger::fmt::Target::Pipe(Box::new(TeeWriter {
            stderr: std::io::stderr(),
            file,
        })))
    })();

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(target) = log_target {
        builder.target(target);
    }
    builder.init();

    // Log panics to notmux.log (otherwise they only go to stderr which is lost)
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("PANIC: {}\n{}", info, backtrace);
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();

    // Parse --remote and --listen flags
    let listen_addr: Option<IpAddr> = {
        if let Some(pos) = args.iter().position(|a| a == "--listen") {
            match args.get(pos + 1) {
                Some(addr_str) => match addr_str.parse::<IpAddr>() {
                    Ok(addr) => Some(addr),
                    Err(_) => {
                        eprintln!("Invalid address for --listen: {addr_str}");
                        eprintln!("Expected an IP address, e.g. --listen 0.0.0.0");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("--listen requires an address argument, e.g. --listen 0.0.0.0");
                    std::process::exit(1);
                }
            }
        } else if args.iter().any(|a| a == "--remote") {
            // --remote without --listen: force-enable server on localhost
            Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        } else {
            None
        }
    };

    // Determine headless mode:
    // 1. Explicit --headless flag
    // 2. Auto-detect on Linux: --listen provided but no DISPLAY/WAYLAND_DISPLAY
    let explicit_headless = args.iter().any(|a| a == "--headless");
    let has_display = std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok();
    let headless =
        explicit_headless || (cfg!(target_os = "linux") && listen_addr.is_some() && !has_display);

    // Acquire instance lock to prevent multiple NotMux processes from
    // clobbering each other's workspace.json.
    let _instance_lock = match persistence::acquire_instance_lock() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if headless {
        if listen_addr.is_none() {
            eprintln!("Headless mode requires --listen <addr>, e.g. --headless --listen 0.0.0.0");
            std::process::exit(1);
        }
        run_headless(listen_addr.expect("listen_addr presence checked above"));
        return;
    }

    if !has_display && cfg!(target_os = "linux") {
        eprintln!("No display server found (DISPLAY/WAYLAND_DISPLAY not set).");
        eprintln!("Use --headless --listen <addr> to run without a GUI.");
        std::process::exit(1);
    }

    Application::with_platform(gpui_platform::current_platform(false)).with_assets(Assets).run(move |cx: &mut App| {
        // Quit the app when the last window is closed (default on macOS is to keep running)
        cx.set_quit_mode(QuitMode::LastWindowClosed);

        // Register action handlers for menu items
        cx.on_action(quit);
        cx.on_action(about);

        // Set up macOS application menu
        set_app_menus(cx);

        // Register embedded JetBrains Mono font
        cx.text_system()
            .add_fonts(embedded_fonts())
            .expect("Failed to register embedded fonts");

        // Register keybindings
        keybindings::register_keybindings(cx);

        // Initialize toast notification system
        cx.set_global(ToastManager::new());

        // Initialize extension registry
        let mut ext_registry = notmux_extensions::ExtensionRegistry::new();
        ext_registry.register(notmux_ext_claude::register());
        ext_registry.register(notmux_ext_codex::register());
        ext_registry.register(notmux_ext_updater::register());
        cx.set_global(ext_registry);

        // Initialize updater (sets GlobalUpdateInfo global, cleans old binary)
        notmux_ext_updater::init(env!("CARGO_PKG_VERSION"), cx);

        // Register theme provider for extensions
        cx.set_global(notmux_extensions::GlobalThemeProvider(|cx| {
            crate::theme::theme(cx)
        }));

        // Register extension settings store (bridge for extensions and view crates to read/write settings).
        // Known namespaces ("terminal", "git") map to/from individual AppSettings fields.
        // Unknown namespaces fall back to the generic extension_settings map.
        cx.set_global(notmux_extensions::ExtensionSettingsStore::new(
            |namespace, cx| {
                let s = settings::settings_entity(cx).read(cx);
                match namespace {
                    "terminal" => {
                        serde_json::to_value(&notmux_views_terminal::TerminalViewSettings {
                            font_size: s.settings.font_size,
                            line_height: s.settings.line_height,
                            font_family: s.settings.font_family.clone(),
                            cursor_style: s.settings.cursor_style,
                            cursor_blink: s.settings.cursor_blink,
                            show_focused_border: s.settings.show_focused_border,
                            show_notification_label: s.settings.show_notification_label,
                            show_shell_selector: s.settings.show_shell_selector,
                            idle_timeout_secs: s.settings.idle_timeout_secs,
                            color_tinted_background: s.settings.color_tinted_background,
                            monochrome_icons: s.settings.monochrome_icons,
                            file_opener: s.settings.file_opener.clone(),
                            default_shell: s.settings.default_shell.clone(),
                            hooks: s.settings.hooks.clone(),
                            persist_scrollback: s.settings.persist_scrollback,
                            persist_scrollback_lines: s.settings.persist_scrollback_lines,
                            terminal_env: s.settings.terminal_env.clone(),
                            terminal_working_directory: s.settings.terminal_working_directory.clone(),
                            option_as_meta: s.settings.option_as_meta,
                        }).ok()
                    }
                    "git" => {
                        let is_dark = crate::theme::theme(cx).is_dark();
                        serde_json::to_value(&notmux_views_git::settings::GitViewSettings {
                            diff_view_mode: s.settings.diff_view_mode,
                            diff_ignore_whitespace: s.settings.diff_ignore_whitespace,
                            diff_font_size: s.settings.diff_font_size,
                            file_font_size: s.settings.file_font_size,
                            monochrome_icons: s.settings.monochrome_icons,
                            is_dark,
                        }).ok()
                    }
                    _ => {
                        s.settings.extension_settings.get(namespace).cloned()
                    }
                }
            },
            |namespace, value, cx| {
                match namespace {
                    "terminal" => {
                        if let Ok(tvs) = serde_json::from_value::<notmux_views_terminal::TerminalViewSettings>(value) {
                            settings::settings_entity(cx).update(cx, |state, cx| {
                                state.settings.font_size = tvs.font_size;
                                state.settings.line_height = tvs.line_height;
                                state.settings.font_family = tvs.font_family;
                                state.settings.cursor_style = tvs.cursor_style;
                                state.settings.cursor_blink = tvs.cursor_blink;
                                state.settings.show_focused_border = tvs.show_focused_border;
                                state.settings.show_notification_label = tvs.show_notification_label;
                                state.settings.show_shell_selector = tvs.show_shell_selector;
                                state.settings.idle_timeout_secs = tvs.idle_timeout_secs;
                                state.settings.color_tinted_background = tvs.color_tinted_background;
                                state.settings.monochrome_icons = tvs.monochrome_icons;
                                state.settings.file_opener = tvs.file_opener;
                                state.settings.default_shell = tvs.default_shell;
                                state.settings.hooks = tvs.hooks;
                                state.settings.persist_scrollback = tvs.persist_scrollback;
                                state.settings.persist_scrollback_lines =
                                    tvs.persist_scrollback_lines.min(50_000);
                                state.settings.terminal_env = tvs.terminal_env;
                                state.settings.terminal_working_directory =
                                    tvs.terminal_working_directory;
                                state.settings.option_as_meta = tvs.option_as_meta;
                                state.save_and_notify(cx);
                            });
                        }
                    }
                    "git" => {
                        if let Ok(gs) = serde_json::from_value::<notmux_views_git::settings::GitViewSettings>(value) {
                            settings::settings_entity(cx).update(cx, |state, cx| {
                                state.settings.diff_view_mode = gs.diff_view_mode;
                                state.settings.diff_ignore_whitespace = gs.diff_ignore_whitespace;
                                state.settings.diff_font_size = gs.diff_font_size;
                                state.settings.file_font_size = gs.file_font_size;
                                state.settings.monochrome_icons = gs.monochrome_icons;
                                state.save_and_notify(cx);
                            });
                        }
                    }
                    _ => {
                        settings::settings_entity(cx).update(cx, |state, cx| {
                            state.set_extension_setting(namespace, value, cx);
                        });
                    }
                }
            },
        ));

        // Initialize hook execution monitor
        cx.set_global(workspace::hook_monitor::HookMonitor::new());

        // Initialize global settings entity (must be before workspace load)
        let settings_entity = settings::init_settings(cx);
        let mut app_settings = settings_entity.read(cx).get().clone();
        if let Some(active_session) = app_settings.active_session.clone()
            && !persistence::session_exists(&active_session)
        {
            settings_entity.update(cx, |state, cx| {
                state.settings.active_session = None;
                state.save_and_notify(cx);
            });
            app_settings.active_session = None;
        }

        // Load or create workspace
        let workspace_data = persistence::load_active_workspace(&app_settings).unwrap_or_else(|e| {
            log::error!("Failed to load workspace: {}. A backup may have been saved to {:?}. Using default workspace.", e, persistence::get_workspace_path().with_extension("json.bak"));
            settings_entity.update(cx, |state, cx| {
                state.settings.active_session = None;
                state.save_and_notify(cx);
            });
            let backup_path = persistence::get_workspace_path().with_extension("json.bak");
            ToastManager::post(
                Toast::error(format!(
                    "Workspace file was corrupted. A backup was saved to {}. \
                     Starting with default workspace. Auto-save is disabled to protect your data — \
                     restart the app after fixing the file.",
                    backup_path.display()
                ))
                    .with_ttl(std::time::Duration::from_secs(30)),
                cx,
            );
            persistence::default_workspace()
        });

        // Restore the persisted theme (falling back to the default if unset or
        // invalid) and install it as the GpuiTheme global.
        let gpui_theme = notmux_theme::load_gpui_theme(&app_settings.theme)
            .or_else(|e| {
                log::warn!(
                    "Failed to load theme '{}': {}. Falling back to '{}'.",
                    app_settings.theme,
                    e,
                    crate::theme::DEFAULT_THEME_NAME
                );
                notmux_theme::load_gpui_theme(crate::theme::DEFAULT_THEME_NAME)
            })
            .expect("default theme must load");
        cx.set_global(gpui_theme);
        *crate::theme::color_snapshot().lock() = crate::theme::theme(cx);

        // OSC color query resolver — reads a snapshot kept in sync with the active theme
        // so terminal apps querying `OSC 10/11/4 ; ? ST` get the real terminal colors.
        {
            let snapshot = crate::theme::color_snapshot().clone();
            notmux_terminal::terminal::register_color_resolver(Arc::new(move |index: usize| {
                let t = snapshot.lock();
                match index {
                    0 => t.term_black,
                    1 => t.term_red,
                    2 => t.term_green,
                    3 => t.term_yellow,
                    4 => t.term_blue,
                    5 => t.term_magenta,
                    6 => t.term_cyan,
                    7 => t.term_white,
                    8 => t.term_bright_black,
                    9 => t.term_bright_red,
                    10 => t.term_bright_green,
                    11 => t.term_bright_yellow,
                    12 => t.term_bright_blue,
                    13 => t.term_bright_magenta,
                    14 => t.term_bright_cyan,
                    15 => t.term_bright_white,
                    256 => t.term_foreground,
                    257 => t.term_background,
                    258 => t.cursor,
                    _ => t.term_foreground,
                }
            }));
        }

        // Register theme provider for notmux-files crate
        cx.set_global(notmux_files::theme::GlobalThemeProvider(|cx| {
            crate::theme::theme(cx)
        }));

        // Initialize explorer clipboard (cut/copy/paste in sidebar file explorer)
        cx.set_global(notmux_files::clipboard::ExplorerClipboard::default());

        // Register UI font size provider for all crates
        cx.set_global(notmux_ui::tokens::GlobalUiFontSize(|cx| {
            settings::settings_entity(cx).read(cx).settings.ui_font_size
        }));

        // NOTE: Terminal and git view settings are now served through
        // ExtensionSettingsStore (registered above) — no separate globals needed.

        // Create PTY manager with session backend from settings
        let (pty_manager, pty_events) = PtyManager::new(app_settings.session_backend);
        let pty_manager = Arc::new(pty_manager);

        // Create the main window
        cx.open_window(
            WindowOptions {
                // On Windows, disable platform titlebar entirely for custom titlebar
                // On macOS, use transparent titlebar with native traffic lights
                titlebar: if cfg!(target_os = "windows") {
                    None
                } else {
                    Some(TitlebarOptions {
                        title: Some("NotMux".into()),
                        appears_transparent: true,
                        // Vertically centre the macOS traffic-light buttons in
                        // our 42 px titlebar. Default position sits them at the
                        // top, leaving them off-axis vs. our own action icons.
                        traffic_light_position: Some(Point {
                            x: px(13.0),
                            y: px(14.0),
                        }),
                    })
                },
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: match (app_settings.window.x, app_settings.window.y) {
                        (Some(x), Some(y)) => Point { x: px(x), y: px(y) },
                        _ => Point::default(),
                    },
                    size: size(px(app_settings.window.width), px(app_settings.window.height)),
                })),
                is_resizable: true,
                // macOS: disable AppKit's native titlebar dragging (it owns a
                // ~42px drag region over our tab strip and steals tab-drag
                // gestures). We move the window ourselves via start_window_move.
                // (Matches zed PR #59836.)
                is_movable: !cfg!(target_os = "macos"),
                // On Windows, use client-side decorations for custom window controls
                window_decorations: Some(if cfg!(target_os = "windows") {
                    WindowDecorations::Client
                } else {
                    WindowDecorations::Server
                }),
                window_min_size: Some(Size {
                    width: px(400.0),
                    height: px(300.0),
                }),
window_background: if app_settings.transparent_background { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque },
                app_id: Some(
                    if cfg!(debug_assertions) { "notmux-dev" } else { "notmux" }.to_string(),
                ),
                ..Default::default()
            },
            |window, cx| {
                // Initialize gpui-component, following the active theme's
                // light/dark base (the theme drives appearance, not the OS).
                gpui_component::init(cx);
                let is_dark = cx.global::<GpuiTheme>().bg_base().l <= 0.5;
                let gpui_mode = if is_dark { GpuiThemeMode::Dark } else { GpuiThemeMode::Light };
                GpuiComponentTheme::change(gpui_mode, Some(window), cx);

                // Wire up content pane registration so PTY events can notify terminal views
                notmux_views_terminal::set_register_content_pane_fn(Box::new(|terminal_id, weak_content| {
                    crate::views::root::content_pane_registry().lock().insert(terminal_id, weak_content);
                }));

                // Create the main app view wrapped in Root (required for gpui_component inputs)
                let notmux = cx.new(|cx| {
                    NotMux::new(workspace_data, pty_manager.clone(), pty_events, listen_addr, window, cx)
                });
                cx.new(|cx| Root::new(notmux, window, cx))
            },
        )
        .expect("Failed to create main window");

        // Flush pending saves on ALL quit paths (including window X button).
        // The Quit action handler only runs for Ctrl+Q / menu quit, not for
        // QuitMode::LastWindowClosed. on_app_quit fires for every exit path.
        cx.on_app_quit(|cx| {
            // Flush pending settings save
            if let Some(gs) = cx.try_global::<GlobalSettings>() {
                gs.0.read(cx).flush_pending_save();
            }

            crate::terminal::snapshot_persist::save_all_snapshots(
                cx,
                notmux_terminal::terminal::SnapshotReason::AppQuit,
            );

            // Flush pending workspace save
            if let Some(gw) = cx.try_global::<GlobalWorkspace>() {
                let active_session = cx
                    .try_global::<GlobalSettings>()
                    .and_then(|gs| gs.0.read(cx).settings.active_session.clone());
                if let Err(e) = persistence::save_active_workspace(
                    gw.0.read(cx).data(),
                    active_session.as_deref(),
                ) {
                    log::error!("Failed to flush workspace on quit: {}", e);
                }
            }
            async {}
        }).detach();
    });
}
