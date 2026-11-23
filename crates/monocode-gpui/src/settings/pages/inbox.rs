use crate::settings::SettingsView;
use gpui::{AnyElement, Context};
impl SettingsView {
    /// Services the Inbox reads are connected in Settings → Integrations.
    pub(super) fn inbox_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![self.project_notification_settings(cx)]
    }
}
