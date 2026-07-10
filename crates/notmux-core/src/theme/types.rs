use serde::{Deserialize, Serialize};

/// Folder color options for projects
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FolderColor {
    #[default]
    Default,
    Red,
    Orange,
    Yellow,
    Lime,
    Green,
    Teal,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Pink,
}

impl FolderColor {
    /// Get all folder color variants for UI
    pub fn all() -> &'static [FolderColor] {
        &[
            FolderColor::Default,
            FolderColor::Red,
            FolderColor::Orange,
            FolderColor::Yellow,
            FolderColor::Lime,
            FolderColor::Green,
            FolderColor::Teal,
            FolderColor::Cyan,
            FolderColor::Blue,
            FolderColor::Indigo,
            FolderColor::Purple,
            FolderColor::Pink,
        ]
    }
}
