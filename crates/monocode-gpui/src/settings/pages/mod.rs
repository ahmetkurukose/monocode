//! One file per Settings page, each a port of its SettingsView.tsx function.
mod appearance;
mod archive;
mod browser_import;
mod chat;
mod connections;
mod extensions;
mod general;
mod inbox;
mod integrations;
mod keybindings;
mod mcp;
mod mobile;
mod monos;
mod passwords;
mod providers;
mod site_permissions;
mod skills;
mod terminal;
mod worktrees;

pub(super) use appearance::parse_hex;
pub(crate) use browser_import::ImportPanel;
pub(crate) use appearance::{ACCENTS, swatch};
pub(super) use connections::ConnectionsState;
pub(super) use extensions::ExtensionsState;
pub(super) use integrations::IntegrationsState;
pub(crate) use keybindings::{command_shortcut, shortcut_label};
pub(super) use mobile::MobileState;
pub(super) use passwords::PasswordsState;

use super::{Section, SettingsView};
use gpui::{AnyElement, Context, Window};

impl SettingsView {
    pub(super) fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.section {
            Section::General => self.general_page(cx),
            Section::Connections => {
                // As on the earlier desktop and in the phone app's copy:
                // Settings → Connections holds both machines and phones.
                let mut page = self.connections_page(cx);
                page.extend(self.mobile_page(cx));
                page
            }
            Section::Integrations => self.integrations_page(cx),
            Section::Appearance => self.appearance_page(window, cx),
            Section::Terminal => self.terminal_page(cx),
            Section::Keybindings => self.keybindings_page(cx),
            Section::Passwords => self.passwords_page(window, cx),
            Section::Extensions => self.extensions_page(cx),
            Section::Chat => self.chat_page(cx),
            Section::Providers => self.providers_page(cx),
            Section::Mcp => self.mcp_page(cx),
            Section::Skills => self.skills_page(cx),
            Section::Monos => self.monos_page(cx),
            Section::Inbox => self.inbox_page(cx),
            Section::Archive => self.archive_page(cx),
            Section::Worktrees => self.worktrees_page(cx),
        }
    }
}
