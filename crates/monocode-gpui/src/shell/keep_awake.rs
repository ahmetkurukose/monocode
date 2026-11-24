//! The footer's Keep computer awake button and its menu, and the feed of
//! working agents into `crate::keep_awake`. The menu bar icon's menu sets the
//! same mode through `Shell::set_keep_awake`.
use super::*;
use crate::keep_awake::{self, KeepAwake, Mode};

const ACTIVE: u32 = 0x34d399;

impl Shell {
    /// Takes the mode from the preferences, once at start and on each change.
    pub(super) fn apply_keep_awake(&mut self, cx: &mut Context<Self>) {
        let mode = Mode::parse(&self.workspace.preferences.keep_awake);
        let before = keep_awake::status(cx);
        cx.default_global::<KeepAwake>().set_mode(mode);
        if keep_awake::status(cx) != before {
            self.keep_awake_changed(cx);
        }
    }

    /// Feeds the working agents in: every session and Mono chat that is
    /// responding, whatever project it is in. Runs with the Dock badge, so
    /// also once a second while the window is hidden.
    pub(super) fn observe_keep_awake(&mut self, cx: &mut Context<Self>) {
        let agents: Vec<_> = self
            .notification_chats(cx)
            .iter()
            .filter_map(|view| {
                let chat = view.read(cx);
                (chat.responding() && !chat.summary.archived)
                    .then(|| (chat.summary.id, chat.transcript.read(cx).activity_mark()))
            })
            .collect();
        let before = keep_awake::status(cx);
        cx.default_global::<KeepAwake>().observe(
            std::time::Instant::now(),
            agents,
            keep_awake::on_battery(),
        );
        if keep_awake::status(cx) != before {
            self.keep_awake_changed(cx);
        }
    }

    /// From the footer menu or the menu bar icon: saves the mode.
    pub(crate) fn set_keep_awake(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.workspace.preferences.keep_awake == mode.key() {
            return;
        }
        self.workspace.preferences.keep_awake = mode.key().into();
        self.apply_preferences(cx);
        self.persist(cx);
        self.sync_settings(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Redraws the button, the open menu and the menu bar icon's menu.
    fn keep_awake_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self
            .menu
            .as_ref()
            .filter(|m| m.target == MenuTarget::KeepAwake)
        {
            let menu = open.menu.clone();
            let items = self.keep_awake_menu(cx);
            menu.update(cx, |menu, cx| menu.replace_items(items, cx));
        }
        if let Some(monos) = self.monos.clone() {
            monos.update(cx, |monos, cx| monos.keep_awake_changed(cx));
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    pub(super) fn keep_awake_menu(&self, cx: &App) -> Vec<MenuItem> {
        let status = keep_awake::status(cx);
        let mut items = vec![
            MenuItem::Heading("Keep computer awake".into(), status.summary().into()),
            MenuItem::Separator,
        ];
        for mode in Mode::ALL {
            let chosen = mode == status.mode;
            items.push(
                MenuAction::new(format!("mode:{}", mode.key()), mode.label())
                    .description(mode.description())
                    .icon("dot")
                    // Every row keeps the dot's room, so the titles line up.
                    .icon_color(if chosen {
                        content(1.)
                    } else {
                        gpui::transparent_black()
                    })
                    .checked(chosen)
                    .into(),
            );
        }
        if let Some(warning) = status.lid_warning() {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Note(warning.into()));
        }
        items
    }

    pub(super) fn run_keep_awake_menu(&mut self, action: &str, cx: &mut Context<Self>) {
        if let Some(mode) = action.strip_prefix("mode:") {
            self.set_keep_awake(Mode::parse(mode), cx);
        }
    }

    /// The footer's button: the mode, and a green dot while it holds.
    pub(super) fn keep_awake_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = keep_awake::status(cx);
        let open = self.menu_open(&MenuTarget::KeepAwake);
        let classic = self.workspace.boxy_layout();
        let description = match (status.mode, status.active) {
            (Mode::Agent, true) => format!(
                "Awake while {} agent{} work{}",
                status.working,
                if status.working == 1 { "" } else { "s" },
                if status.working == 1 { "s" } else { "" }
            ),
            _ => status.summary(),
        };
        div()
            .id("footer-keep-awake")
            .debug_selector(|| "footer-keep-awake".into())
            .role(Role::Button)
            .aria_label(SharedString::from(format!(
                "Keep computer awake. {}",
                status.mode.label()
            )))
            .aria_description(SharedString::from(description))
            .aria_expanded(open)
            .relative()
            .h(px(20.))
            .px(px(if classic { 6. } else { 8. }))
            .map(|d| {
                if classic {
                    d.rounded_md()
                } else {
                    d.rounded_full()
                }
            })
            .mr(px(terminal_dock::FOOTER_GAP))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .whitespace_nowrap()
            .text_size(px(11.))
            .cursor_pointer()
            .text_color(content(if open { 1. } else { 0.55 }))
            .when(open, |d| d.bg(content(0.08)))
            .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
            .child(self.keep_awake_anchor.probe())
            .child(icon("coffee", 12.).text_color(content(0.55)))
            .child(status.mode.label())
            .when(status.active, |d| {
                d.child(div().size(px(6.)).rounded_full().bg(rgb(ACTIVE)))
            })
            .child(icon("chevron-down", 12.).text_color(content(0.45)))
            .on_click(cx.listener(|this, _, window, cx| {
                if this.menu_open(&MenuTarget::KeepAwake) {
                    this.close_menu(cx);
                } else {
                    let anchor = Anchor::Slot(this.keep_awake_anchor.clone());
                    this.open_menu(MenuTarget::KeepAwake, anchor, window, cx);
                }
            }))
            .into_any_element()
    }
}
