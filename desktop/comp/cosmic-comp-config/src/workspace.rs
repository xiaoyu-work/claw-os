// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::EdidProduct;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub workspace_mode: WorkspaceMode,
    #[serde(default)]
    pub workspace_layout: WorkspaceLayout,
    #[serde(default)]
    pub action_on_typing: Action,
    #[serde(default = "default_wraparound")]
    pub workspace_wraparound: bool,
}

fn default_wraparound() -> bool {
    true
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            workspace_mode: WorkspaceMode::default(),
            workspace_layout: WorkspaceLayout::default(),
            action_on_typing: Action::default(),
            workspace_wraparound: default_wraparound(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceMode {
    #[default]
    OutputBound,
    Global,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceLayout {
    Vertical,
    /// Workspaces run left-to-right, and the overview shows them as a strip
    /// across the top of the screen — the arrangement macOS uses for Spaces,
    /// and the one the overview reads best in: thumbnails are landscape, so a
    /// horizontal strip costs one row of height instead of a column down the
    /// side of the screen.
    #[default]
    Horizontal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[default]
    None,
    OpenLauncher,
    OpenApplications,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMatch {
    pub name: String,
    pub edid: Option<EdidProduct>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedWorkspace {
    pub output: OutputMatch,
    pub tiling_enabled: bool,
    pub id: Option<String>,
    // TODO: name
}
