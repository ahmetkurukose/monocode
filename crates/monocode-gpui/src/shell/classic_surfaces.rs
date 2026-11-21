//! The original layout's Settings replaces the project workspace while open.
use super::*;

impl Shell {
    /// Settings replaces the original layout's workspace while it is open.
    /// App views, Search and Monos are tabs in the workspace instead.
    pub(super) fn classic_surface_open(&self, _: &App) -> bool {
        self.workspace.classic_layout && self.settings.is_some()
    }

    pub(super) fn session_sidebar_available(&self, cx: &App) -> bool {
        // The tree lists sessions under their projects, so its column goes.
        !self.classic_surface_open(cx) && !self.project_rail_tree_on()
    }

    pub(super) fn classic_rail_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        } else {
            self.visit(-1, window, cx);
        }
    }
}
