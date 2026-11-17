//! Starting a debug session from the interface: the launcher the run
//! feature's Debug actions reach, and the notice shown when a program
//! cannot be debugged (it is not Python, or `debugpy` is not installed in
//! the project's environment). Nothing is installed for the user; the
//! notice carries the command to copy.
use super::sessions;
use crate::{
    code_shell::{
        self,
        context::{self, Checkout},
        dock::{self, DockTab},
        launch,
    },
    theme::*,
};
use gpui::{
    App, AppContext, ClipboardItem, Context, Role, SharedString, Window, div, prelude::*, px,
};
use monocode_core::code::{
    dap::{self, Actor, Console, DapError, LaunchOptions},
    services::services,
    spawn::Spawn,
    toolchain::Toolchain,
};
use std::{path::PathBuf, rc::Rc};

const NOTICE_TAB: &str = "debug-notice";

pub(super) fn init(cx: &mut App) {
    launch::set_debugger(Rc::new(start), cx);
}

/// The program a file is debugged as: the checkout's interpreter on it.
pub(super) fn file_spawn(path: &str, checkout: &Checkout) -> Spawn {
    let name = std::path::Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    Spawn {
        label: name,
        command: "python".into(),
        args: vec![path.to_owned()],
        cwd: PathBuf::from(&checkout.folder),
        env: Vec::new(),
    }
}

/// Debugs what `spawn` would run, with the debuggee in a terminal tab.
/// The adapter is started off the interface thread; the session's tab
/// appears when the manager lists it.
pub(super) fn start(mut spawn: Spawn, checkout: Checkout, window: &mut Window, cx: &mut App) {
    if spawn.cwd.as_os_str().is_empty() {
        spawn.cwd = PathBuf::from(&checkout.folder);
    }
    let options = LaunchOptions {
        console: Console::IntegratedTerminal,
        ..LaunchOptions::default()
    };
    if dap::derive(&spawn, &options).is_none() {
        let message = format!(
            "\"{}\" cannot be debugged: only Python programs can for now.",
            spawn.label
        );
        show_notice(message, None, checkout, window, cx);
        return;
    }
    let setup = context::terminal_setup(&checkout.project, cx);
    let (folder, project) = (
        PathBuf::from(&checkout.folder),
        PathBuf::from(&checkout.project),
    );
    let started = cx.background_executor().spawn(async move {
        // Finding the virtual environment reads the disk.
        let toolchain = Toolchain::for_folder(&setup, &folder, &project);
        services()
            .dap()
            .start(&spawn, &toolchain, &options, Actor::User)
    });
    cx.spawn(async move |cx| {
        let started = started.await;
        cx.update(|cx| match started {
            Ok(session) => {
                if let Some(debugger) = sessions::debugger(cx) {
                    debugger.update(cx, |debugger, cx| debugger.focus(session.id(), cx));
                }
            }
            Err(error) => {
                let (message, command) = explain(&error);
                code_shell::in_window(cx, |window, cx| {
                    show_notice(message, command, checkout, window, cx)
                });
            }
        });
    })
    .detach();
}

/// A failed start as a sentence, with the command that would fix it.
pub(super) fn explain(error: &DapError) -> (String, Option<String>) {
    match error {
        DapError::ToolMissing { tool, hint } => {
            let command = (hint.split_once(": "))
                .map(|(_, command)| command.trim().to_owned())
                .filter(|command| command.contains("install"));
            (
                format!("{tool} was not found, so the program cannot be debugged. {hint}"),
                command,
            )
        }
        other => (other.to_string(), None),
    }
}

/// Why debugging did not start, as a dock tab the user can read and close.
pub(super) struct Notice {
    message: SharedString,
    command: Option<SharedString>,
    copied: bool,
}
impl Render for Notice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("debug-start-notice")
            .debug_selector(|| "debug-start-notice".into())
            .role(Role::Alert)
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(px(13.))
            .text_color(content(0.9))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(icon("circle-alert", 16.).text_color(gpui::rgb(0xfbbf24)))
                    .child(div().min_w_0().flex_1().child(self.message.clone())),
            )
            .children(self.command.clone().map(|command| {
                let text = command.to_string();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("debug-install-command")
                            .debug_selector(|| "debug-install-command".into())
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(content(0.07))
                            .font_family("monospace")
                            .text_size(px(12.))
                            .child(command),
                    )
                    .child(
                        div()
                            .id("debug-copy-command")
                            .debug_selector(|| "debug-copy-command".into())
                            .role(Role::Button)
                            .aria_label("Copy the command")
                            .flex_shrink_0()
                            .h(px(24.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded_md()
                            .bg(content(0.1))
                            .cursor_pointer()
                            .hover(|style| style.bg(content(0.16)))
                            .child(if self.copied { "Copied" } else { "Copy" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                this.copied = true;
                                cx.notify();
                            })),
                    )
            }))
    }
}

/// Shows the notice in the project's dock, in place of an earlier one.
pub(super) fn show_notice(
    message: String,
    command: Option<String>,
    checkout: Checkout,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(old) = dock::find_tab(&checkout.project, NOTICE_TAB, cx) {
        old.close(window, cx);
    }
    let view = cx.new(|_| Notice {
        message: message.into(),
        command: command.map(Into::into),
        copied: false,
    });
    let _ = dock::add_tab(
        DockTab {
            id: NOTICE_TAB.into(),
            title: "Debug".into(),
            icon: "bug",
            view: view.into(),
            focus: None,
            controls: None,
            checkout,
            on_close: None,
            on_placement: None,
            reveal: true,
        },
        window,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_debugpy_is_explained_with_the_command_to_run() {
        let (message, command) = explain(&DapError::ToolMissing {
            tool: "debugpy".into(),
            hint: "Install it in the project's environment: /p/.venv/bin/python -m pip install debugpy".into(),
        });
        assert!(message.starts_with("debugpy was not found"));
        assert_eq!(
            command.as_deref(),
            Some("/p/.venv/bin/python -m pip install debugpy")
        );
        let (_, none) = explain(&DapError::ToolMissing {
            tool: "python".into(),
            hint: "Create a virtual environment in the project or put python3 on the PATH.".into(),
        });
        assert_eq!(none, None);
        let (plain, none) = explain(&DapError::Start("The adapter did not start.".into()));
        assert_eq!((plain.as_str(), none), ("The adapter did not start.", None));
    }
}
