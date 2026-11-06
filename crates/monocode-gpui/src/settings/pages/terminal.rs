//! TerminalPage: how terminals read and what selecting in them does.
use crate::settings::{SettingsView, SliderKind};
use gpui::{AnyElement, Context};

impl SettingsView {
    pub(super) fn terminal_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.prefs.clone();
        let text = vec![self.row(
            Some("terminal-font-size"),
            true,
            "Font size",
            Some("The size of the text in every terminal. The grid refits, so a smaller size shows more.".into()),
            vec![self.slider(
                SliderKind::TerminalFontSize,
                format!("{} pt", p.terminal_font_size),
                false,
                cx,
            )],
        )];
        let selection = vec![self.row(
            Some("terminal-copy-on-select"),
            true,
            "Copy on select",
            Some("Selecting text copies it to the clipboard, as in iTerm2. Off, copy with the shortcut or the context menu.".into()),
            vec![self.toggle(
                "terminal-copy-on-select",
                "Copy on select",
                p.terminal_copy_on_select,
                false,
                cx,
                |this, on, cx| this.update(cx, |p| p.terminal_copy_on_select = on),
            )],
        )];
        vec![
            self.group(None, true, "Text", None, None, text),
            self.group(None, false, "Selection", None, None, selection),
        ]
    }
}
