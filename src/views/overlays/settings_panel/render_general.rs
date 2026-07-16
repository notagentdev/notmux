use crate::settings::settings_entity;
use crate::theme::theme;
use crate::ui::tokens::{ui_text, ui_text_sm};
use crate::views::components::simple_input::SimpleInput;
use gpui::prelude::*;
use gpui::*;
use gpui_component::v_flex;
use super::SettingsPanel;
use super::components::*;
impl SettingsPanel {
    pub(super) fn render_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let s = settings_entity(cx).read(cx).settings.clone();
        let appearance_toggles = section_container(&t)
            .child(self.render_toggle(
                "focus-border", "Show Focus Border", s.show_focused_border, true,
                |state, val, cx| state.set_show_focused_border(val, cx), cx,
            ))
            .child(self.render_toggle_with_desc(
                "notification-label",
                "Show Notification Label",
                "Show the agent's message as a label on unfocused panes (ring + badge always show)",
                s.show_notification_label,
                true,
                |state, val, cx| state.set_show_notification_label(val, cx),
                cx,
            ))
            .child(self.render_toggle_with_desc(
                "native-notifications",
                "System Notifications",
                "Post native OS notifications when an agent needs attention while NotMux is in the background",
                s.native_notifications,
                true,
                |state, val, cx| state.set_native_notifications(val, cx),
                cx,
            ))
            .child(self.render_toggle(
                "color-tinted-bg", "Color Tinted Background", s.color_tinted_background, true,
                |state, val, cx| state.set_color_tinted_background(val, cx), cx,
            ))
            .child(self.render_toggle(
                "monochrome-icons", "Monochrome Icons", s.monochrome_icons, true,
                |state, val, cx| state.set_monochrome_icons(val, cx), cx,
            ))
            .child(self.render_toggle_with_desc(
                "transparent-background",
                "Transparent Background",
                "Blur and transparency effect (restart required to fully apply)",
                s.transparent_background,
                true,
                |state, val, cx| state.set_transparent_background(val, cx),
                cx,
            ))
            .child(self.render_toggle(
                "show-hidden-files",
                "Show Hidden Files in Explorer",
                s.file_explorer.show_hidden,
                true,
                |state, val, cx| state.set_file_explorer_show_hidden(val, cx),
                cx,
            ))
            .child(self.render_number_stepper(
                "min-col-width", "Min Column Width", s.min_column_width,
                "{}px", 50.0, 60.0, false,
                |state, val, cx| state.set_min_column_width(val, cx), cx,
            ));
        let remote_section = section_container(&t)
            .child(self.render_toggle(
                "remote-server", "Remote Server", s.remote_server_enabled, true,
                |state, val, cx| state.set_remote_server_enabled(val, cx), cx,
            ))
            .child(self.render_toggle_with_desc(
                "agent-hooks",
                "Agent Notifications",
                "Install hooks so Claude Code, Codex & shell notify notmux when a turn finishes",
                s.agent_hooks_enabled,
                true,
                |state, val, cx| state.set_agent_hooks_enabled(val, cx),
                cx,
            ))
            .child(self.render_toggle_with_desc(
                "auto-resume-agent-sessions",
                "Auto-Resume Agent Sessions",
                "After a restart, resume the agent session (claude --resume, codex resume, …) that was running in each restored terminal. Requires Agent Notifications.",
                s.auto_resume_agent_sessions,
                true,
                |state, val, cx| state.set_auto_resume_agent_sessions(val, cx),
                cx,
            ))
            .when(s.remote_server_enabled, |d| {
                d.child(
                    div()
                        .px(px(16.0))
                        .py(px(14.0))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .border_b_1()
                        .border_color(rgb(t.border))
                        .child(
                            v_flex()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_size(ui_text(13.0, cx))
                                        .text_color(rgb(t.text_primary))
                                        .child("Listen Address"),
                                )
                                .child(
                                    div()
                                        .text_size(ui_text_sm(cx))
                                        .text_color(rgb(t.text_muted))
                                        .child("IP address to bind the remote server (e.g. 0.0.0.0 for all interfaces)"),
                                ),
                        )
                        .child(
                            div()
                                .bg(rgb(t.bg_primary))
                                .border_1()
                                .border_color(rgb(t.border))
                                .rounded(px(4.0))
                                .child(SimpleInput::new(&self.listen_address_input).text_size(ui_text(13.0, cx))),
                        ),
                )
            });
        let file_opener_section = section_container(&t).child(
            div()
                .px(px(16.0))
                .py(px(14.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    v_flex()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(ui_text(13.0, cx))
                                .text_color(rgb(t.text_primary))
                                .child("Editor Command"),
                        )
                        .child(
                            div()
                                .text_size(ui_text_sm(cx))
                                .text_color(rgb(t.text_muted))
                                .child("Command to open file paths (empty = system default)"),
                        ),
                )
                .child(
                    div()
                        .bg(rgb(t.bg_primary))
                        .border_1()
                        .border_color(rgb(t.border))
                        .rounded(px(4.0))
                        .child(SimpleInput::new(&self.file_opener_input).text_size(ui_text(13.0, cx))),
                ),
        );
        v_flex()
            .gap(px(24.0))
            .child(section_header("General", &t, cx))
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(subsection_label("APPEARANCE", &t, cx))
                    .child(appearance_toggles),
            )
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(subsection_label("REMOTE", &t, cx))
                    .child(remote_section),
            )
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(subsection_label("FILE OPENER", &t, cx))
                    .child(file_opener_section),
            )
    }
    }
