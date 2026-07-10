//! A 1px vertical divider with a wider, overlapping grab hitbox — the resizable
//! splitter pattern used by notmux. The visible line stays 1px while the hitbox
//! extends a few pixels to each side so it is easy to grab.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::*;

/// Visible divider thickness.
const DIVIDER_SIZE: f32 = 1.0;
/// Grabbable hitbox thickness (overlaps the neighbouring content).
const HANDLE_HITBOX_SIZE: f32 = 9.0;

/// Splitter orientation: a `Vertical` divider resizes width (drag left/right);
/// a `Horizontal` divider resizes height (drag up/down).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

/// A draggable splitter. On mouse-down it invokes `on_drag_start`; the caller is
/// responsible for applying the drag (e.g. via top-level mouse-move).
pub struct ResizeHandle {
    color: Hsla,
    active_color: Hsla,
    orientation: Orientation,
    #[allow(clippy::type_complexity)]
    on_drag_start: Rc<RefCell<Option<Box<dyn FnOnce(Point<Pixels>, &mut App)>>>>,
}

impl ResizeHandle {
    /// Creates a vertical splitter (resizes width) with the given idle/active colors.
    pub fn new(
        color: Hsla,
        active_color: Hsla,
        on_drag_start: impl FnOnce(Point<Pixels>, &mut App) + 'static,
    ) -> Self {
        Self::with_orientation(Orientation::Vertical, color, active_color, on_drag_start)
    }

    /// Creates a horizontal splitter (resizes height).
    pub fn horizontal(
        color: Hsla,
        active_color: Hsla,
        on_drag_start: impl FnOnce(Point<Pixels>, &mut App) + 'static,
    ) -> Self {
        Self::with_orientation(Orientation::Horizontal, color, active_color, on_drag_start)
    }

    fn with_orientation(
        orientation: Orientation,
        color: Hsla,
        active_color: Hsla,
        on_drag_start: impl FnOnce(Point<Pixels>, &mut App) + 'static,
    ) -> Self {
        Self {
            color,
            active_color,
            orientation,
            on_drag_start: Rc::new(RefCell::new(Some(Box::new(on_drag_start)))),
        }
    }
}

impl IntoElement for ResizeHandle {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ResizeHandle {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        match self.orientation {
            Orientation::Vertical => {
                style.size.width = px(DIVIDER_SIZE).into();
                style.size.height = relative(1.0).into();
            }
            Orientation::Horizontal => {
                style.size.width = relative(1.0).into();
                style.size.height = px(DIVIDER_SIZE).into();
            }
        }
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        // Expand the hitbox across the divider so the 1px line is easy to grab.
        let expand = px((HANDLE_HITBOX_SIZE - DIVIDER_SIZE) / 2.0);
        let hitbox_bounds = match self.orientation {
            Orientation::Vertical => Bounds::new(
                point(bounds.origin.x - expand, bounds.origin.y),
                size(px(HANDLE_HITBOX_SIZE), bounds.size.height),
            ),
            Orientation::Horizontal => Bounds::new(
                point(bounds.origin.x, bounds.origin.y - expand),
                size(bounds.size.width, px(HANDLE_HITBOX_SIZE)),
            ),
        };
        window.insert_hitbox(hitbox_bounds, HitboxBehavior::BlockMouseExceptScroll)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let hovered = hitbox.is_hovered(window);
        let color = if hovered {
            self.active_color
        } else {
            self.color
        };
        window.paint_quad(fill(bounds, color));
        let cursor = match self.orientation {
            Orientation::Vertical => CursorStyle::ResizeLeftRight,
            Orientation::Horizontal => CursorStyle::ResizeUpDown,
        };
        window.set_cursor_style(cursor, hitbox);

        // The app doesn't repaint on idle, so the hover highlight wouldn't update
        // on its own. Request a redraw whenever the hover state changes (both when
        // entering and leaving the splitter), tracking the last painted state.
        let hover_hitbox = hitbox.id;
        let last_hover = Rc::new(Cell::new(hovered));
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, _cx| {
            if phase == DispatchPhase::Bubble {
                let now = hover_hitbox.is_hovered(window);
                if now != last_hover.get() {
                    last_hover.set(now);
                    window.refresh();
                }
            }
        });

        let on_drag_start = self.on_drag_start.clone();
        let hitbox_id = hitbox.id;
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && e.button == MouseButton::Left
                && hitbox_id.is_hovered(window)
            {
                if let Some(cb) = on_drag_start.borrow_mut().take() {
                    cb(e.position, cx);
                }
                cx.stop_propagation();
            }
        });
    }
}
