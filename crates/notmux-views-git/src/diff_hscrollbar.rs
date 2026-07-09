//! A draggable horizontal scrollbar for an inline diff block, bound to a
//! [`DiffViewer`]'s horizontal scroll state. Rendered as the last row of a
//! file's expanded diff so it sits at the bottom of the block.

use gpui::*;

use crate::diff_viewer::DiffViewer;

/// Height of the scrollbar row.
pub const HSCROLLBAR_HEIGHT: f32 = 10.0;
/// Minimum thumb width so it stays grabbable.
const MIN_THUMB: f32 = 28.0;

/// Drag state: `(mouse_x_at_press, scroll_x_at_press)`.
#[derive(Default)]
pub struct DiffHScrollbarDrag(Option<(Pixels, f32)>);

impl Global for DiffHScrollbarDrag {}

/// A horizontal scrollbar element bound to a diff viewer's `scroll_x`.
pub struct DiffHScrollbar {
    viewer: Entity<DiffViewer>,
    thumb_color: Hsla,
    thumb_hover_color: Hsla,
}

impl DiffHScrollbar {
    pub fn new(viewer: Entity<DiffViewer>, thumb: Hsla, thumb_hover: Hsla) -> Self {
        Self { viewer, thumb_color: thumb, thumb_hover_color: thumb_hover }
    }
}

pub struct DiffHScrollbarPrepaint {
    thumb: Option<Bounds<Pixels>>,
    /// Pixels of thumb travel for the full scroll range.
    span: Pixels,
    /// Max horizontal scroll offset (px).
    max_off: f32,
    hitbox: Hitbox,
}

impl IntoElement for DiffHScrollbar {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for DiffHScrollbar {
    type RequestLayoutState = ();
    type PrepaintState = DiffHScrollbarPrepaint;

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
        style.size.width = relative(1.).into();
        style.size.height = px(HSCROLLBAR_HEIGHT).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let v = self.viewer.read(cx);
        let max_off = v.inline_max_scroll_x();
        let content_w = v.inline_content_width();
        let viewport_w = v.inline_viewport_width();
        let scroll_x = v.inline_scroll_x();

        let track_w = bounds.size.width;
        let (thumb, span) = if max_off > 0.5 && content_w > 0.0 && track_w > px(0.) {
            let frac_visible = (viewport_w / content_w).clamp(0.05, 1.0);
            let thumb_w = (track_w * frac_visible).max(px(MIN_THUMB)).min(track_w);
            let span = track_w - thumb_w;
            let pos = (scroll_x / max_off).clamp(0.0, 1.0);
            let thumb_left = bounds.left() + span * pos;
            let thumb = Bounds::new(
                point(thumb_left, bounds.top() + px(2.)),
                size(thumb_w, bounds.size.height - px(4.)),
            );
            (Some(thumb), span)
        } else {
            (None, px(0.))
        };

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        DiffHScrollbarPrepaint { thumb, span, max_off, hitbox }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if cx.try_global::<DiffHScrollbarDrag>().is_none() {
            cx.set_global(DiffHScrollbarDrag::default());
        }

        let Some(thumb) = prepaint.thumb else {
            return; // no horizontal overflow → nothing to draw
        };

        let dragging = cx
            .try_global::<DiffHScrollbarDrag>()
            .is_some_and(|d| d.0.is_some());
        let hovered = thumb.contains(&window.mouse_position());
        let thumb_color = if hovered || dragging {
            self.thumb_hover_color
        } else {
            self.thumb_color
        };
        window.paint_quad(gpui::quad(
            thumb,
            Corners::all(px(3.)),
            thumb_color,
            Edges::default(),
            gpui::transparent_black(),
            BorderStyle::default(),
        ));
        window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);

        let span = prepaint.span;
        let max_off = prepaint.max_off;

        // Press: jump the thumb to the click (if on track) then grab it.
        {
            let viewer = self.viewer.clone();
            let track_left = bounds.left();
            let thumb_w = thumb.size.width;
            window.on_mouse_event(
                move |event: &MouseDownEvent, phase, window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble || !bounds.contains(&event.position) {
                        return;
                    }
                    let cur = viewer.read(cx).inline_scroll_x();
                    if thumb.contains(&event.position) {
                        *cx.global_mut::<DiffHScrollbarDrag>() =
                            DiffHScrollbarDrag(Some((event.position.x, cur)));
                    } else {
                        let target = (event.position.x - track_left - thumb_w / 2.).clamp(px(0.), span);
                        let new = if span > px(0.) {
                            (f32::from(target) / f32::from(span)) * max_off
                        } else {
                            0.0
                        };
                        viewer.update(cx, |v, cx| v.set_inline_scroll_x(new, cx));
                        *cx.global_mut::<DiffHScrollbarDrag>() =
                            DiffHScrollbarDrag(Some((event.position.x, new)));
                    }
                    window.refresh();
                },
            );
        }

        // Drag.
        {
            let viewer = self.viewer.clone();
            window.on_mouse_event(
                move |event: &MouseMoveEvent, phase, window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let Some((down_x, x_at_down)) = cx.global::<DiffHScrollbarDrag>().0 else {
                        return;
                    };
                    if span <= px(0.) {
                        return;
                    }
                    let delta = f32::from(event.position.x - down_x);
                    let new = x_at_down + (delta / f32::from(span)) * max_off;
                    viewer.update(cx, |v, cx| v.set_inline_scroll_x(new, cx));
                    window.refresh();
                },
            );
        }

        // Release.
        {
            window.on_mouse_event(
                move |_event: &MouseUpEvent, phase, _window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    if cx.global::<DiffHScrollbarDrag>().0.is_some() {
                        *cx.global_mut::<DiffHScrollbarDrag>() = DiffHScrollbarDrag(None);
                    }
                },
            );
        }
    }
}
