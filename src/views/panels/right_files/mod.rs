//! Optimized file explorer + file viewer for the tabbed right panel, ported
//! from the notagent reference (anti-flicker, virtualized directory tree).
//!
//! Only `file_explorer` is wired into the app today (the git panel's Files
//! tab). The standalone `files_panel` host with its context menu and splitter
//! is not constructed anywhere — kept for a future standalone files window.
#[allow(dead_code)]
pub mod explorer_context_menu;
pub mod file_explorer;
#[allow(dead_code)]
pub mod files_panel;
#[allow(dead_code)]
pub mod resize_handle;
