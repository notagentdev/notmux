//! Hosts a native Wry webview as a GPUI element, clipped to what GPUI would
//! show.
//!
//! A Wry webview is a native child view of the window: it floats above every
//! GPUI-painted pixel and ignores `overflow_hidden`. Placed at its laid-out
//! bounds alone, a partly scrolled-out browser pane would bleed over the
//! sidebar, neighbouring project columns, or terminal panes. This host
//! therefore positions the native view at the intersection of its bounds
//! with the current content mask — the clip region GPUI applies to its own
//! content — and hides it entirely while that intersection is empty.
//!
//! Visibility has two independent inputs: the pane's logical decision
//! (`show`/`hide`: active tab, project on screen, no overlay) and the clip
//! state computed at prepaint. The native view is shown only when both agree.

use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, Entity, FocusHandle, Focusable, GlobalElementId,
    Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement, LayoutId,
    MouseDownEvent, ParentElement as _, Pixels, Render, Size, Style, Styled as _, Window, div, px,
};
use wry::Rect;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size as DpiSize};

/// A GPUI entity owning a native Wry webview.
pub struct WebViewHost {
    focus_handle: FocusHandle,
    webview: Rc<wry::WebView>,
    /// The pane wants the view on screen (its layout slot is visible).
    visible: bool,
    /// The last prepaint found no visible area inside the content mask.
    clipped: bool,
}

impl Drop for WebViewHost {
    fn drop(&mut self) {
        self.hide();
    }
}

impl WebViewHost {
    /// Wrap `webview`, which must have been built as a child of the window.
    pub fn new(webview: wry::WebView, _window: &mut Window, cx: &mut App) -> Self {
        let _ = webview.set_bounds(Rect::default());
        Self {
            focus_handle: cx.focus_handle(),
            webview: Rc::new(webview),
            visible: true,
            clipped: false,
        }
    }

    /// Let the pane show the view (it still stays hidden while clipped away).
    pub fn show(&mut self) {
        self.visible = true;
        self.apply_native_visibility();
    }

    /// Hide the view and hand keyboard focus back to the window.
    pub fn hide(&mut self) {
        let _ = self.webview.focus_parent();
        self.visible = false;
        self.apply_native_visibility();
    }

    /// Whether the pane currently wants the view shown.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Navigate the page to `url`.
    pub fn load_url(&mut self, url: &str) {
        let _ = self.webview.load_url(url);
    }

    /// The raw Wry webview (script evaluation, snapshots).
    pub fn raw(&self) -> &wry::WebView {
        &self.webview
    }

    fn set_clipped(&mut self, clipped: bool) {
        if self.clipped != clipped {
            self.clipped = clipped;
            self.apply_native_visibility();
        }
    }

    fn apply_native_visibility(&self) {
        let _ = self.webview.set_visible(self.visible && !self.clipped);
    }
}

impl Focusable for WebViewHost {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WebViewHost {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(WebViewElement {
                host: cx.entity(),
                webview: self.webview.clone(),
            })
    }
}

/// The element that places the native view during prepaint.
struct WebViewElement {
    host: Entity<WebViewHost>,
    webview: Rc<wry::WebView>,
}

impl IntoElement for WebViewElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebViewElement {
    type RequestLayoutState = ();
    /// The clipped bounds the native view was placed at, when it is shown.
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if !self.host.read(cx).visible() {
            return None;
        }

        // Only the part inside every ancestor's clip region may show.
        let visible = bounds.intersect(&window.content_mask().bounds);
        let clipped = visible.size.width <= px(0.) || visible.size.height <= px(0.);
        self.host.update(cx, |host, _| host.set_clipped(clipped));
        if clipped {
            return None;
        }

        let _ = self.webview.set_bounds(Rect {
            size: DpiSize::Logical(LogicalSize {
                width: visible.size.width.into(),
                height: visible.size.height.into(),
            }),
            position: Position::Logical(LogicalPosition::new(
                visible.origin.x.into(),
                visible.origin.y.into(),
            )),
        });

        Some(window.insert_hitbox(visible, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(hitbox) = hitbox else {
            return;
        };
        let bounds = hitbox.bounds;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let webview = self.webview.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, _, _, _| {
                if !bounds.contains(&event.position) {
                    // A click outside the page blurs its input focus.
                    let _ = webview.focus_parent();
                }
            });
        });
    }
}
