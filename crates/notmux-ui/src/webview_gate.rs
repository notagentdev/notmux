//! Global suspend gate for native webviews.
//!
//! Wry webviews are native child views that float *above* all GPUI content.
//! Full-window overlays (settings panel, modals, viewers) would otherwise be
//! covered by them. The root view sets this gate while such an overlay is
//! open; every webview-hosting pane polls it and hides its webview while the
//! gate is closed.

use std::sync::atomic::{AtomicBool, Ordering};

static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// Suspend (or resume) all embedded webviews. Called by the root view when a
/// full-window overlay opens/closes.
pub fn set_webviews_suspended(suspended: bool) {
    SUSPENDED.store(suspended, Ordering::Relaxed);
}

/// Whether webviews are currently suspended by an overlay.
pub fn webviews_suspended() -> bool {
    SUSPENDED.load(Ordering::Relaxed)
}
