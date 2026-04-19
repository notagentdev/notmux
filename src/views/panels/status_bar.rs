use crate::settings::settings_entity;
use crate::theme::theme;
use crate::workspace::state::Workspace;
use crate::ui::tokens::{ui_text_ms, ui_text_sm};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::h_flex;
use okena_extensions::{ExtensionInstance, ExtensionRegistry};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Disks, System};
use time::OffsetDateTime;

/// Refresh interval for system stats
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Cached system stats
#[derive(Clone, Default)]
struct SystemStats {
    cpu_usage: f32,
    memory_used_gb: f32,
    memory_total_gb: f32,
    disk_free_gb: f32,
    disk_total_gb: f32,
}

/// Global system info cache
struct SystemInfoCache {
    system: System,
    disks: Disks,
    stats: SystemStats,
}

impl SystemInfoCache {
    fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();

        let disks = Disks::new_with_refreshed_list();

        Self {
            system,
            disks,
            stats: SystemStats::default(),
        }
    }

    fn refresh(&mut self) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.disks.refresh(true);

        // Calculate average CPU usage across all cores
        let cpu_usage = self.system.cpus().iter()
            .map(|cpu| cpu.cpu_usage())
            .sum::<f32>() / self.system.cpus().len().max(1) as f32;

        let memory_used = self.system.used_memory() as f64 / 1_073_741_824.0; // bytes to GB
        let memory_total = self.system.total_memory() as f64 / 1_073_741_824.0;

        // Pick the primary disk: the one mounted at the shortest root path
        // ("/" on Unix, "C:\" on Windows). This excludes tmpfs/overlay mounts
        // whose paths tend to be deeper.
        let primary = self.disks.list().iter()
            .filter(|d| !d.is_removable())
            .min_by_key(|d| d.mount_point().as_os_str().len());
        let (disk_free, disk_total) = match primary {
            Some(d) => (
                d.available_space() as f64 / 1_073_741_824.0,
                d.total_space() as f64 / 1_073_741_824.0,
            ),
            None => (0.0, 0.0),
        };

        self.stats = SystemStats {
            cpu_usage,
            memory_used_gb: memory_used as f32,
            memory_total_gb: memory_total as f32,
            disk_free_gb: disk_free as f32,
            disk_total_gb: disk_total as f32,
        };
    }

    fn stats(&self) -> SystemStats {
        self.stats.clone()
    }
}

/// Status bar component showing system info and time
pub struct StatusBar {
    cache: Arc<Mutex<SystemInfoCache>>,
    /// Activate functions cloned from registry (keyed by extension ID).
    activate_fns: Vec<(String, okena_extensions::ActivateFn)>,
    /// Active extension instances. Dropping an instance deactivates the extension
    /// (cancels background tasks, releases views).
    active_extensions: HashMap<String, ExtensionInstance>,
    sidebar_open: bool,
}

impl StatusBar {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let cache = Arc::new(Mutex::new(SystemInfoCache::new()));

        // Initial refresh
        cache.lock().refresh();

        // Start periodic refresh
        let cache_for_task = cache.clone();
        cx.spawn(async move |this: WeakEntity<StatusBar>, cx| {
            loop {
                smol::Timer::after(REFRESH_INTERVAL).await;

                // Refresh system info
                cache_for_task.lock().refresh();

                // Notify to re-render
                let result = this.update(cx, |_this, cx| {
                    cx.notify();
                });

                if result.is_err() {
                    break; // View was dropped
                }
            }
        }).detach();

        // Clone activate functions from the global registry.
        let activate_fns: Vec<_> = cx.try_global::<ExtensionRegistry>()
            .map(|registry| {
                registry.extensions().iter()
                    .map(|ext| (ext.manifest.id.to_string(), ext.activate.clone()))
                    .collect()
            })
            .unwrap_or_default();

        // Activate initially enabled extensions
        let enabled = settings_entity(cx).read(cx).settings.enabled_extensions.clone();
        let active_extensions = Self::activate_extensions(&activate_fns, &enabled, cx);

        // Observe settings to sync extensions when enabled_extensions changes
        let settings = settings_entity(cx);
        cx.observe(&settings, |this, entity, cx| {
            let enabled = entity.read(cx).settings.enabled_extensions.clone();
            this.sync_extensions(&enabled, cx);
        }).detach();

        // Re-render when workspace changes (for potential UI updates)
        cx.observe(&workspace, |_, _, cx| cx.notify()).detach();

        Self {
            cache, activate_fns, active_extensions, sidebar_open: true,
        }
    }

    /// Activate extensions that are in the enabled set.
    fn activate_extensions(
        activate_fns: &[(String, okena_extensions::ActivateFn)],
        enabled: &HashSet<String>,
        cx: &mut App,
    ) -> HashMap<String, ExtensionInstance> {
        activate_fns.iter()
            .filter(|(id, _)| enabled.contains(id.as_str()))
            .map(|(id, activate)| (id.clone(), activate(cx)))
            .collect()
    }

    /// Sync active extensions with the current enabled set.
    /// Activates newly enabled extensions, deactivates disabled ones
    /// (dropping the instance cancels background tasks and releases views).
    fn sync_extensions(&mut self, enabled: &HashSet<String>, cx: &mut Context<Self>) {
        // Deactivate disabled (drop instances → cancel tasks)
        self.active_extensions.retain(|id, _| enabled.contains(id.as_str()));

        // Activate newly enabled
        for (id, activate) in &self.activate_fns {
            if enabled.contains(id.as_str()) && !self.active_extensions.contains_key(id) {
                self.active_extensions.insert(id.clone(), activate(cx));
            }
        }

        cx.notify();
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    fn format_time() -> String {
        match OffsetDateTime::now_local() {
            Ok(now) => format!("{:02}:{:02}", now.hour(), now.minute()),
            Err(_) => {
                // Fallback to UTC if local time is unavailable
                let now = OffsetDateTime::now_utc();
                format!("{:02}:{:02}", now.hour(), now.minute())
            }
        }
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let stats = self.cache.lock().stats();

        // Get current time using chrono-free approach
        let time_str = Self::format_time();

        // Format memory
        let memory_str = format!("{:.1}/{:.1} GB", stats.memory_used_gb, stats.memory_total_gb);
        let memory_percent = if stats.memory_total_gb > 0.0 {
            (stats.memory_used_gb / stats.memory_total_gb * 100.0) as u32
        } else {
            0
        };

        let cpu_color = if stats.cpu_usage > 80.0 {
            t.metric_critical
        } else if stats.cpu_usage > 50.0 {
            t.metric_warning
        } else {
            t.metric_normal
        };

        let mem_color = if memory_percent > 80 {
            t.metric_critical
        } else if memory_percent > 60 {
            t.metric_warning
        } else {
            t.metric_normal
        };

        // Disk: warn when usage climbs (free space shrinks)
        let disk_used_percent = if stats.disk_total_gb > 0.0 {
            ((stats.disk_total_gb - stats.disk_free_gb) / stats.disk_total_gb * 100.0) as u32
        } else {
            0
        };
        let disk_color = if disk_used_percent > 90 {
            t.metric_critical
        } else if disk_used_percent > 75 {
            t.metric_warning
        } else {
            t.metric_normal
        };
        let disk_str = format!("{:.0}/{:.0} GB", stats.disk_free_gb, stats.disk_total_gb);

        // Collect widgets in stable registry order from active extensions
        let left_widgets: Vec<&Vec<AnyView>> = self.activate_fns.iter()
            .filter_map(|(id, _)| self.active_extensions.get(id))
            .map(|inst| &inst.status_bar_widgets)
            .filter(|w| !w.is_empty())
            .collect();
        let right_widgets: Vec<&Vec<AnyView>> = self.activate_fns.iter()
            .filter_map(|(id, _)| self.active_extensions.get(id))
            .map(|inst| &inst.status_bar_right_widgets)
            .filter(|w| !w.is_empty())
            .collect();

        div()
            .id("status-bar")
            .h(px(22.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(t.bg_header))
            .border_t_1()
            .border_color(rgb(t.border))
            .text_size(ui_text_ms(cx))
            // Left side — system stats (sidebar toggle now lives in the titlebar)
            .child({
                let mut left = h_flex().gap(px(16.0))
                    // CPU
                    .child(
                        h_flex()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_color(rgb(t.text_muted))
                                    .child("CPU")
                            )
                            .child(
                                div()
                                    .text_color(rgb(cpu_color))
                                    .child(format!("{:02.0}%", stats.cpu_usage))
                            )
                    )
                    // Memory
                    .child(
                        h_flex()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_color(rgb(t.text_muted))
                                    .child("MEM")
                            )
                            .child(
                                div()
                                    .text_color(rgb(mem_color))
                                    .child(memory_str)
                            )
                    )
                    // Free disk space (free / total)
                    .child(
                        h_flex()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_color(rgb(t.text_muted))
                                    .child("DISK")
                            )
                            .child(
                                div()
                                    .text_color(rgb(disk_color))
                                    .child(disk_str)
                            )
                    );

                // Left-side extension widgets
                for widgets in &left_widgets {
                    for widget in *widgets {
                        left = left.child(widget.clone());
                    }
                }

                left
            })
            // Right side - remote info + version + time
            .child({
                let mut right = h_flex()
                    .gap(px(8.0));

                // Right-side extension widgets
                for widgets in &right_widgets {
                    for widget in *widgets {
                        right = right.child(widget.clone());
                    }
                }

                // Show remote server status if active
                if let Some(remote_info) = cx.try_global::<crate::remote::GlobalRemoteInfo>()
                    && let Some(port) = remote_info.0.port() {
                        right = right.child(
                            div()
                                .id("remote-info")
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_color(rgb(t.term_cyan))
                                        .child(format!("REMOTE :{}", port))
                                )
                                .child(
                                    div()
                                        .id("pair-btn")
                                        .cursor_pointer()
                                        .px(px(6.0))
                                        .py(px(1.0))
                                        .rounded(px(3.0))
                                        .text_color(rgb(t.term_yellow))
                                        .text_size(ui_text_sm(cx))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .hover(|s| s.bg(rgb(t.bg_hover)))
                                        .child("Pair")
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(
                                                Box::new(crate::keybindings::ShowPairingDialog),
                                                cx,
                                            );
                                        })
                                )
                        );
                    }

                // Zoom controls
                let settings_for_minus = settings_entity(cx);
                let settings_for_plus = settings_entity(cx);
                let current_ui_size = settings_entity(cx).read(cx).settings.ui_font_size;

                right = right.child(
                    h_flex()
                        .gap(px(2.0))
                        .items_center()
                        .child(
                            div()
                                .id("zoom-out-btn")
                                .cursor_pointer()
                                .px(px(4.0))
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .text_color(rgb(t.text_secondary))
                                .hover(|s| s.bg(rgb(t.bg_hover)).text_color(rgb(t.text_primary)))
                                .child("−")
                                .on_click(move |_, _window, cx| {
                                    settings_for_minus.update(cx, |state, cx| {
                                        state.set_ui_font_size(state.settings.ui_font_size - 1.0, cx);
                                    });
                                }),
                        )
                        .child(
                            div()
                                .text_color(rgb(t.text_muted))
                                .text_size(ui_text_sm(cx))
                                .child(format!("{:.0}", current_ui_size)),
                        )
                        .child(
                            div()
                                .id("zoom-in-btn")
                                .cursor_pointer()
                                .px(px(4.0))
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .text_color(rgb(t.text_secondary))
                                .hover(|s| s.bg(rgb(t.bg_hover)).text_color(rgb(t.text_primary)))
                                .child("+")
                                .on_click(move |_, _window, cx| {
                                    settings_for_plus.update(cx, |state, cx| {
                                        state.set_ui_font_size(state.settings.ui_font_size + 1.0, cx);
                                    });
                                }),
                        ),
                );

                right
                    .when(cfg!(not(target_os = "macos")), |el| {
                        el.child(
                            div()
                                .text_color(rgb(t.text_muted))
                                .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                        )
                    })
                    .child(
                        div()
                            .text_color(rgb(t.text_secondary))
                            .child(time_str)
                    )
            })
    }
}
