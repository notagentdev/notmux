use super::{Cursor, DisplayMode, FileViewer, FileViewerTab};
use gpui::{ClipboardItem, Context, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px, size};
use std::path::PathBuf;

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(notmux_theme::load_gpui_theme("dark").unwrap());
    });
}

fn viewer(cx: &mut Context<FileViewer>) -> FileViewer {
    let fs = std::sync::Arc::new(crate::project_fs::LocalProjectFs::new(
        PathBuf::from("/nonexistent-notmux-ui-test"),
    ));
    let mut viewer = FileViewer::new_browse(
        fs, 14.0, true, notmux_core::theme::DARK_THEME, false, cx,
    );
    viewer.embedded = true;
    let mut tab = FileViewerTab::new_loading(PathBuf::from("test.md"), None);
    tab.apply_loaded_content(
        Ok("# Heading\n\nA paragraph to scroll.\n\n".repeat(80)),
        &viewer.syntax_set,
        &viewer.theme_colors,
    );
    viewer.tabs = vec![tab];
    viewer
}

#[gpui::test]
fn raw_toggle_and_typing(cx: &mut TestAppContext) {
    init(cx);
    let (view, cx) = cx.add_window_view(|_, cx| viewer(cx));
    let toggle = cx.debug_bounds("display-mode-toggle").expect("embedded Markdown needs a visible toggle");
    cx.simulate_click(toggle.center(), Modifiers::default());
    assert!(view.read_with(cx, |v, _| v.active_tab().display_mode == DisplayMode::Source));
    cx.simulate_keystrokes("r b");
    assert!(view.read_with(cx, |v, _| v.active_tab().buffer.text().starts_with("rb# Heading")));
    cx.simulate_click(toggle.center(), Modifiers::default());
    assert!(view.read_with(cx, |v, _| v.active_tab().display_mode == DisplayMode::Preview));
}

#[gpui::test]
fn mouse_and_trackpad_scroll_without_switching(cx: &mut TestAppContext) {
    init(cx);
    let (view, cx) = cx.add_window_view(|_, cx| viewer(cx));
    cx.simulate_resize(size(px(600.0), px(400.0)));
    cx.run_until_parked();
    for mode in [DisplayMode::Preview, DisplayMode::Source] {
        view.update(cx, |v, cx| { v.active_tab_mut().display_mode = mode; cx.notify(); });
        cx.run_until_parked();
        let handle = view.read_with(cx, |v, _| {
            if mode == DisplayMode::Preview {
                v.active_tab().markdown_scroll_handle.clone()
            } else {
                v.active_tab().source_scroll_handle.0.borrow().base_handle.clone()
            }
        });
        assert!(handle.max_offset().y > px(0.0));
        for delta in [
            ScrollDelta::Pixels(point(px(0.0), px(-100.0))),
            ScrollDelta::Lines(point(0.0, -3.0)),
            ScrollDelta::Pixels(point(px(0.0), px(-100.0))),
        ] {
            let before = handle.offset().y;
            cx.simulate_event(ScrollWheelEvent {
                position: point(px(200.0), px(200.0)), delta,
                modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
            assert!(handle.offset().y < before);
        }
    }
}

#[gpui::test]
fn context_menu_cut_paste_preserves_unicode_and_selection(cx: &mut TestAppContext) {
    init(cx);
    let (view, cx) = cx.add_window_view(|_, cx| viewer(cx));
    view.update(cx, |v, cx| {
        v.toggle_display_mode(cx);
        v.select_all(cx);
        v.insert_text_at_cursor("aé\nz end", cx);
        v.active_tab_mut().selection.start = Some((0, 1));
        v.active_tab_mut().selection.end = Some((1, 1));
    });
    cx.run_until_parked();
    cx.simulate_mouse_down(point(px(200.0), px(200.0)), MouseButton::Right, Modifiers::default());
    let cut = cx.debug_bounds("fv-editor-cut").expect("right-click must open editor actions");
    cx.simulate_click(cut.center(), Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.active_tab().buffer.text().to_string()), "a end");
    assert_eq!(cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())), Some("é\nz".to_string()));
    cx.simulate_mouse_down(point(px(200.0), px(200.0)), MouseButton::Right, Modifiers::default());
    let paste = cx.debug_bounds("fv-editor-paste").unwrap();
    cx.simulate_click(paste.center(), Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.active_tab().buffer.text().to_string()), "aé\nz end");
    assert!(cx.debug_bounds("fv-editor-context-menu").is_none());
}

#[gpui::test]
fn preview_is_read_only_and_menu_dismisses(cx: &mut TestAppContext) {
    init(cx);
    let (view, cx) = cx.add_window_view(|_, cx| viewer(cx));
    let before = view.read_with(cx, |v, _| v.active_tab().buffer.text().to_string());
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("replacement".into())));
    view.update(cx, |v, cx| {
        v.select_all_markdown(cx);
        v.cut_selection(cx);
        v.paste_clipboard(cx);
        assert_eq!(v.active_tab().cursor, Cursor::default());
    });
    assert_eq!(view.read_with(cx, |v, _| v.active_tab().buffer.text().to_string()), before);
    cx.simulate_mouse_down(point(px(200.0), px(200.0)), MouseButton::Right, Modifiers::default());
    assert!(cx.debug_bounds("fv-editor-context-menu").is_some());
    cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
    assert!(cx.debug_bounds("fv-editor-context-menu").is_none());
}
