//! A draggable vertical scrollbar element bound to a gpui [`ListState`].
//!
//! Reads the list's scrollbar geometry and paints a track + grabbable thumb on
//! the right edge, writing the scroll position back while dragged. Ported from
//! the notagent desktop scrollbar so the vendored git panel can show one without
//! depending on the host crate.

use gpui::*;

/// Minimum thumb height so it stays grabbable on long lists.
const MIN_THUMB: f32 = 24.0;

/// Drag state shared across frames: `(mouse_y_at_press, scroll_offset_at_press)`.
#[derive(Default)]
pub struct ListScrollbarDrag(Option<(Pixels, Pixels)>);

impl Global for ListScrollbarDrag {}

/// A vertical scrollbar element bound to a [`ListState`].
pub struct ListScrollbar {
    list: ListState,
    track_color: Hsla,
    thumb_color: Hsla,
    thumb_hover_color: Hsla,
}

impl ListScrollbar {
    /// Create a scrollbar for the given list state and colors.
    pub fn new(list: ListState, track: Hsla, thumb: Hsla, thumb_hover: Hsla) -> Self {
        Self {
            list,
            track_color: track,
            thumb_color: thumb,
            thumb_hover_color: thumb_hover,
        }
    }
}

/// Geometry computed in prepaint and reused for painting + hit-testing.
pub struct ListScrollbarPrepaint {
    track: Bounds<Pixels>,
    thumb: Option<Bounds<Pixels>>,
    max_off: Pixels,
    span: Pixels,
    hitbox: Hitbox,
}

impl IntoElement for ListScrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ListScrollbar {
    type RequestLayoutState = ();
    type PrepaintState = ListScrollbarPrepaint;

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
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let max_off = self.list.max_offset_for_scrollbar().y.max(px(0.));
        let off = (-self.list.scroll_px_offset_for_scrollbar().y).clamp(px(0.), max_off);
        let track_h = bounds.size.height;

        let (thumb, span) = if max_off > px(0.) && track_h > px(0.) {
            let content_h = track_h + max_off;
            let thumb_h = (track_h * (track_h / content_h)).max(px(MIN_THUMB)).min(track_h);
            let span = track_h - thumb_h;
            let frac = (off / max_off).clamp(0., 1.);
            let thumb_top = bounds.top() + span * frac;
            // Thin thumb (the app-wide 5px look), pinned to the right edge;
            // the wider strip stays the click/drag target.
            let thumb_w = px(5.).min(bounds.size.width);
            let thumb = Bounds::new(
                point(bounds.right() - thumb_w, thumb_top),
                size(thumb_w, thumb_h),
            );
            (Some(thumb), span)
        } else {
            (None, px(0.))
        };

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        ListScrollbarPrepaint { track: bounds, thumb, max_off, span, hitbox }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if cx.try_global::<ListScrollbarDrag>().is_none() {
            cx.set_global(ListScrollbarDrag::default());
        }

        let Some(thumb) = prepaint.thumb else {
            return;
        };

        let dragging = cx
            .try_global::<ListScrollbarDrag>()
            .is_some_and(|d| d.0.is_some());

        // Auto-hide (identical to the desktop scrollbar): the bar only shows
        // while the pointer is over the scrollable area it belongs to (or
        // mid-drag). A move handler refreshes exactly on the enter/leave
        // transition, since a hit-test change alone does not repaint.
        let area = self.list.viewport_bounds();
        let visible = dragging || area.contains(&window.mouse_position());
        {
            let was_visible = visible;
            window.on_mouse_event(
                move |event: &MouseMoveEvent, phase, window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let dragging = cx
                        .try_global::<ListScrollbarDrag>()
                        .is_some_and(|d| d.0.is_some());
                    let now_visible = dragging || area.contains(&event.position);
                    if now_visible != was_visible {
                        window.refresh();
                    }
                },
            );
        }
        if !visible {
            return;
        }

        let hovered = thumb.contains(&window.mouse_position());
        let thumb_color = if hovered || dragging {
            self.thumb_hover_color
        } else {
            self.thumb_color
        };
        window.paint_quad(fill(prepaint.track, self.track_color));
        window.paint_quad(gpui::quad(
            thumb,
            Corners::all(px(3.)),
            thumb_color,
            Edges::default(),
            gpui::transparent_black(),
            BorderStyle::default(),
        ));
        window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);

        let track = prepaint.track;
        let span = prepaint.span;
        let max_off = prepaint.max_off;

        // Press: grab the thumb, or jump to the click then grab it.
        {
            let list = self.list.clone();
            window.on_mouse_event(
                move |event: &MouseDownEvent, phase, window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble || !track.contains(&event.position) {
                        return;
                    }
                    let off = (-list.scroll_px_offset_for_scrollbar().y).clamp(px(0.), max_off);
                    list.scrollbar_drag_started();
                    if thumb.contains(&event.position) {
                        *cx.global_mut::<ListScrollbarDrag>() =
                            ListScrollbarDrag(Some((event.position.y, off)));
                    } else {
                        let target = (event.position.y - track.top() - thumb.size.height / 2.)
                            .clamp(px(0.), span);
                        let new_off = if span > px(0.) { (target / span) * max_off } else { px(0.) };
                        list.set_offset_from_scrollbar(point(px(0.), new_off));
                        *cx.global_mut::<ListScrollbarDrag>() =
                            ListScrollbarDrag(Some((event.position.y, new_off)));
                    }
                    window.refresh();
                },
            );
        }

        // Drag: move the scroll proportionally to the pointer's travel.
        {
            let list = self.list.clone();
            window.on_mouse_event(
                move |event: &MouseMoveEvent, phase, window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let Some((down_y, off_at_down)) = cx.global::<ListScrollbarDrag>().0 else {
                        return;
                    };
                    if span <= px(0.) {
                        return;
                    }
                    let delta = event.position.y - down_y;
                    let new_off = (off_at_down + (delta / span) * max_off).clamp(px(0.), max_off);
                    list.set_offset_from_scrollbar(point(px(0.), new_off));
                    window.refresh();
                },
            );
        }

        // Release.
        {
            let list = self.list.clone();
            window.on_mouse_event(
                move |_event: &MouseUpEvent, phase, _window: &mut Window, cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    if cx.global::<ListScrollbarDrag>().0.is_some() {
                        *cx.global_mut::<ListScrollbarDrag>() = ListScrollbarDrag(None);
                        list.scrollbar_drag_ended();
                    }
                },
            );
        }
    }
}
