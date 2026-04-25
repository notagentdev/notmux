mod status;
mod usage;
mod ui_helpers;

use gpui::AppContext as _;
use vryn_extensions::{ExtensionInstance, ExtensionManifest, ExtensionRegistration};
use std::sync::Arc;

pub fn register() -> ExtensionRegistration {
    ExtensionRegistration {
        manifest: ExtensionManifest {
            id: "claude-code",
            name: "Claude Code",
            default_enabled: false,
        },
        activate: Arc::new(|app| {
            let status = app.new(status::ClaudeStatus::new);
            let usage = app.new(usage::ClaudeUsage::new);
            ExtensionInstance {
                status_bar_widgets: vec![status.into(), usage.into()],
                status_bar_right_widgets: vec![],
            }
        }),
        settings_view: None,
    }
}
