//! Layout management views.
//!
//! This module contains views for managing terminal layouts:
//! - Layout containers (tabs, splits)
//! - Split panes with resize handles
//! - Individual terminal panes
//! - Focus navigation between panes

pub mod browser_automation;
pub mod browser_pane;
pub mod browser_registry;
pub mod editor_registry;
pub mod layout_container;
pub mod navigation;
pub mod pane_drag;
pub mod split_pane;
mod tabs;
pub mod terminal_pane;
