//! Process-wide clipboard for cut/copy/paste operations in the sidebar
//! file explorer. Registered as a GPUI global.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardOp {
    Cut,
    Copy,
}

#[derive(Default, Clone, Debug)]
pub struct ExplorerClipboard {
    pub path: Option<PathBuf>,
    pub op: Option<ClipboardOp>,
}

impl ExplorerClipboard {
    pub fn is_set(&self) -> bool {
        self.path.is_some()
    }

    pub fn set(&mut self, path: PathBuf, op: ClipboardOp) {
        self.path = Some(path);
        self.op = Some(op);
    }

    pub fn clear(&mut self) {
        self.path = None;
        self.op = None;
    }
}

impl gpui::Global for ExplorerClipboard {}
