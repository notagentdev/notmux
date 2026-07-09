use gpui::*;
use notmux_core::client::{ConnectionStatus, RemoteConnectionConfig};
use notmux_ui::theme::theme;
use notmux_ui::tokens::{ui_text_md, ui_text_ms, ui_text_sm, ui_text_xl};

use crate::sidebar::Sidebar;

/// Owned snapshot of a single connection for rendering.
struct ConnectionSnapshot {
    config: RemoteConnectionConfig,
    status: ConnectionStatus,
}

impl Sidebar {
    /// Render the REMOTE section (header + connection status headers).
    /// Remote projects are now rendered as regular workspace projects inside auto-created folders,
    /// so this section only needs connection management UI.
    pub fn render_remote_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_remote = self.get_remote_connections.is_some();

        if !has_remote {
            return div().into_any_element();
        }

        // Snapshot connection data via callback
        let snapshots: Vec<ConnectionSnapshot> =
            if let Some(ref get_connections) = self.get_remote_connections {
                (get_connections)(cx)
                    .into_iter()
                    .map(|s| ConnectionSnapshot {
                        config: s.config,
                        status: s.status,
                    })
                    .collect()
            } else {
                Vec::new()
            };

        if snapshots.is_empty() {
            return div()
                .child(self.render_remote_header(cx))
                .into_any_element();
        }

        let mut children: Vec<AnyElement> = Vec::new();
        children.push(self.render_remote_header(cx).into_any_element());

        for snap in &snapshots {
            children.push(
                self.render_connection_header(&snap.config, &snap.status, false, cx)
                    .into_any_element(),
            );
        }

        div().children(children).into_any_element()
    }

    fn render_remote_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let request_broker = self.request_broker.clone();
        div()
            .h(px(32.0))
            .pl(px(12.0))
            // Right padding matches the rows' pr(14) so the "+" lines up in the
            // same column as the per-row eye (visibility) buttons.
            .pr(px(14.0))
            .mt(px(8.0))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        svg()
                            .path("icons/link.svg")
                            .size(px(14.0))
                            .text_color(rgb(t.text_secondary)),
                    )
                    .child(
                        div()
                            .text_size(ui_text_ms(cx))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text_secondary))
                            .child("REMOTE"),
                    ),
            )
            .child(
                div()
                    .id("remote-add-btn")
                    .cursor_pointer()
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(rgb(t.bg_hover)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(ui_text_xl(cx))
                            .text_color(rgb(t.text_secondary))
                            .child("+"),
                    )
                    .on_click(move |_, _window, cx| {
                        request_broker.update(cx, |broker, cx| {
                            broker.push_overlay_request(
                                notmux_workspace::requests::OverlayRequest::RemoteConnect,
                                cx,
                            );
                        });
                        cx.stop_propagation();
                    }),
            )
    }

    fn render_connection_header(
        &self,
        config: &RemoteConnectionConfig,
        status: &ConnectionStatus,
        _is_collapsed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let name = config.name.clone();
        let host_port = format!("{}:{}", config.host, config.port);

        // Status dot color
        let status_color = match status {
            ConnectionStatus::Connected => t.term_green,
            ConnectionStatus::Connecting
            | ConnectionStatus::Pairing
            | ConnectionStatus::Reconnecting { .. } => t.term_yellow,
            ConnectionStatus::Disconnected => t.text_muted,
            ConnectionStatus::Error(_) => t.term_red,
        };

        let status_text = match status {
            ConnectionStatus::Connecting => "Connecting...",
            ConnectionStatus::Pairing => "Pairing...",
            ConnectionStatus::Reconnecting { .. } => "Reconnecting...",
            ConnectionStatus::Disconnected => "Disconnected",
            ConnectionStatus::Error(_) => "Error",
            ConnectionStatus::Connected => "Connected",
        };

        let conn_id_for_ctx = config.id.clone();
        let conn_name_for_ctx = config.name.clone();
        let is_pairing = matches!(
            status,
            ConnectionStatus::Pairing | ConnectionStatus::Error(_) | ConnectionStatus::Disconnected
        );

        div()
            .id(ElementId::Name(format!("remote-conn-{}", config.id).into()))
            .h(px(32.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(t.bg_hover)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    this.request_broker.update(cx, |broker, cx| {
                        broker.push_overlay_request(
                            notmux_workspace::requests::OverlayRequest::RemoteConnectionContextMenu {
                                connection_id: conn_id_for_ctx.clone(),
                                connection_name: conn_name_for_ctx.clone(),
                                is_pairing,
                                position: event.position,
                            },
                            cx,
                        );
                    });
                    cx.stop_propagation();
                }),
            )
            .child(
                // Status dot
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded_full()
                    .bg(rgb(status_color))
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .text_size(ui_text_md(cx))
                    .text_color(rgb(t.text_primary))
                    .child(name),
            )
            .child(
                div()
                    .text_size(ui_text_sm(cx))
                    .text_color(rgb(t.text_muted))
                    .child(format!("{} — {}", host_port, status_text)),
            )
    }

}
