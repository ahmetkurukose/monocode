//! Run, code insight and debug checked from the inside, in a build nobody
//! is using.
//!
//! The interface tests run without a window, real language servers answer
//! in their own time, and what a click lands on depends on a real frame.
//! With `MONOCODE_CODE_SELFTEST` set to a file, a build with the `selftest`
//! feature opens the sample project it was started in (`--cwd`, written by
//! `scripts/code-selftest/sample.py`), sends mouse and key events through
//! the window's own event path, writes what it then found to that file step
//! by step, and quits. `MONOCODE_CODE_SELFTEST_SHOTS` names a folder for
//! pictures of the window at each step. Run it against a throwaway data
//! directory (`MONOCODE_NATIVE_DATA_DIR`), never the user's; see
//! `scripts/code-selftest/README.md`.
use super::*;
use crate::{code_editor, code_shell::context, file_editor::FileEditor, input::Input};
use gpui::{
    AsyncWindowContext, Keystroke, Modifiers, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, WeakEntity, point,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

mod agent;
mod debugging;
mod insight;
mod running;

type Cx = AsyncWindowContext;

/// The window being driven and what has been found so far.
struct Driver {
    shell: WeakEntity<Shell>,
    out: PathBuf,
    shots: Option<PathBuf>,
    /// The sample project, spelled as the shell spells it.
    root: PathBuf,
    steps: Vec<Value>,
    pictures: Vec<Value>,
}

impl Shell {
    pub(super) fn start_code_selftest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(out) = std::env::var_os("MONOCODE_CODE_SELFTEST") else {
            return;
        };
        let root = PathBuf::from(self.workspace.project.clone());
        cx.spawn_in(window, async move |this, cx| {
            let mut driver = Driver {
                shell: this,
                out: PathBuf::from(out),
                shots: std::env::var_os("MONOCODE_CODE_SELFTEST_SHOTS").map(PathBuf::from),
                root,
                steps: Vec::new(),
                pictures: Vec::new(),
            };
            driver.write(false);
            driver.run(cx).await;
            driver.write(true);
            // Held open for the script that ends the app from outside, to
            // see what it leaves behind.
            if std::env::var_os("MONOCODE_CODE_SELFTEST_HOLD").is_none() {
                let _ = cx.update(|_, cx| cx.quit());
            }
        })
        .detach();
    }
}

impl Driver {
    async fn run(&mut self, cx: &mut Cx) {
        if !self.prepare(cx).await {
            return;
        }
        if std::env::var_os("MONOCODE_CODE_SELFTEST_HOLD").is_some() {
            self.hold(cx).await;
            return;
        }
        let only = std::env::var("MONOCODE_CODE_SELFTEST_ONLY").unwrap_or_default();
        let wanted = |part: &str| only.is_empty() || only.split(',').any(|name| name == part);
        if wanted("insight") {
            self.insight(cx).await;
        }
        if wanted("run") {
            self.running(cx).await;
        }
        if wanted("debug") {
            self.debugging(cx).await;
        }
        if wanted("agent") {
            self.agent(cx).await;
        }
        if wanted("idle") {
            self.idle(cx).await;
        }
    }

    /// The window in front with a session of the project, as after the user
    /// opened the project and started a chat.
    async fn prepare(&mut self, cx: &mut Cx) -> bool {
        self.pause(cx, 1500).await;
        // A window behind others is not drawn, and an app that is not in
        // front has no key window to send keys to.
        let _ = cx.update(|window, cx| {
            cx.activate(true);
            window.activate_window();
        });
        self.pause(cx, 800).await;
        self.shell(cx, |shell, _, cx| {
            if shell.terminal_owner().is_none() && shell.project_session_for_terminal().is_none() {
                shell.new_chat(cx);
            }
        });
        self.pause(cx, 800).await;
        let state = self.shell(cx, |shell, window, cx| {
            let size = window.viewport_size();
            let snapshot = context::snapshot(cx);
            json!({
                "project": shell.workspace.project,
                "projects": snapshot.projects,
                "checkouts": snapshot.checkouts.iter().map(|c| c.folder.clone()).collect::<Vec<_>>(),
                "session": shell.terminal_owner().or_else(|| shell.project_session_for_terminal()),
                "viewport": [size.width.as_f32(), size.height.as_f32()],
                "window_active": window.is_window_active(),
                "app_view": shell.app_view_open(),
                "settings": shell.settings.is_some(),
                "debugger": crate::code_shell::launch::can_debug(cx),
            })
        });
        let ready = state.as_ref().is_some_and(|state| {
            state["session"].is_i64() && state["project"].as_str() == self.root.to_str()
        });
        self.record("0", "the sample project is open with a session", ready, json!(state));
        ready
    }

    // --- what was found -------------------------------------------------

    fn record(&mut self, step: &str, name: &str, pass: bool, detail: Value) {
        self.steps
            .push(json!({ "step": step, "name": name, "pass": pass, "detail": detail }));
        self.write(false);
    }

    /// Written after every step, so a run that hangs still says how far it got.
    fn write(&self, finished: bool) {
        let report = json!({
            "finished": finished,
            "steps": self.steps,
            "pictures": self.pictures,
        });
        let _ = std::fs::write(
            &self.out,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        );
    }

    /// A picture of the frame on screen, drawn again off screen: it needs no
    /// screen recording permission and works behind a locked screen.
    async fn shot(&mut self, cx: &mut Cx, name: &str) {
        let Some(folder) = self.shots.clone() else {
            return;
        };
        self.pause(cx, 400).await;
        let path = folder.join(format!("{:02}-{name}.png", self.pictures.len() + 1));
        let image = cx.update(|window, _| window.render_to_image());
        let saved = match image {
            Ok(Ok(image)) => {
                let to = path.clone();
                cx.background_executor()
                    .spawn(async move { image.save(&to).map_err(|error| error.to_string()) })
                    .await
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("The window closed".into()),
        };
        self.pictures.push(match saved {
            Ok(()) => json!({ "file": path }),
            Err(error) => json!({ "file": path, "error": error }),
        });
    }

    // --- the window -----------------------------------------------------

    async fn pause(&self, cx: &mut Cx, millis: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(millis))
            .await;
    }

    fn shell<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) -> R,
    ) -> Option<R> {
        self.shell.update_in(cx, f).ok()
    }

    /// Outside any update of the shell, as a click handler runs.
    fn app<R>(&self, cx: &mut Cx, f: impl FnOnce(&mut Window, &mut App) -> R) -> Option<R> {
        cx.update(f).ok()
    }

    /// Lets real time pass until `done`, for at most `millis`.
    async fn until(
        &self,
        cx: &mut Cx,
        millis: u64,
        mut done: impl FnMut(&mut Window, &mut App) -> bool,
    ) -> bool {
        for _ in 0..=millis / 100 {
            if self.app(cx, &mut done) == Some(true) {
                return true;
            }
            self.pause(cx, 100).await;
        }
        false
    }

    // --- input, through the window's own event path -----------------------

    fn event(&self, cx: &mut Cx, event: PlatformInput) {
        let _ = cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        });
    }

    /// Key presses as written in a key binding: `cmd-p`, `shift-f12 enter`.
    async fn keys(&self, cx: &mut Cx, keys: &str) {
        for key in keys.split(' ') {
            if let Ok(keystroke) = Keystroke::parse(key) {
                let _ = cx.update(|window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                });
            }
            self.pause(cx, 80).await;
        }
    }

    /// Text typed a character at a time.
    async fn type_text(&self, cx: &mut Cx, text: &str) {
        for character in text.chars() {
            let keystroke = Keystroke {
                modifiers: Modifiers {
                    shift: character.is_uppercase(),
                    ..Modifiers::default()
                },
                key: match character {
                    ' ' => "space".into(),
                    other => other.to_lowercase().to_string(),
                },
                key_char: Some(character.to_string()),
            };
            let _ = cx.update(|window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            });
            self.pause(cx, 60).await;
        }
    }

    async fn move_to(&self, cx: &mut Cx, position: Point<Pixels>, modifiers: Modifiers) {
        self.event(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                modifiers,
                pressed_button: None,
            }),
        );
        self.pause(cx, 60).await;
    }

    async fn press(
        &self,
        cx: &mut Cx,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
    ) {
        self.move_to(cx, position, modifiers).await;
        self.event(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                position,
                modifiers,
                button,
                click_count: 1,
                first_mouse: false,
            }),
        );
        self.pause(cx, 40).await;
        self.event(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                modifiers,
                button,
                click_count: 1,
            }),
        );
        self.pause(cx, 150).await;
    }

    async fn click(&self, cx: &mut Cx, position: Point<Pixels>) {
        self.press(cx, position, MouseButton::Left, Modifiers::default())
            .await;
    }

    /// Where the element with this `debug_selector` was drawn.
    fn bounds(&self, cx: &mut Cx, selector: &str) -> Option<Bounds<Pixels>> {
        cx.update(|window, _| window.debug_bounds(selector))
            .ok()
            .flatten()
    }

    /// Clicks the middle of a drawn element; false when it is not drawn.
    async fn click_on(&self, cx: &mut Cx, selector: &str) -> bool {
        let Some(bounds) = self.bounds(cx, selector) else {
            return false;
        };
        self.click(cx, bounds.center()).await;
        true
    }

    /// Puts away what a fresh data directory shows over the panes (the
    /// notice of a harness update lies over the file header's buttons).
    async fn calm(&self, cx: &mut Cx) {
        self.shell(cx, |shell, _, cx| {
            let harness = shell.notification_state.harness.clone();
            harness.update(cx, |harness, cx| harness.dismiss(cx));
        });
        self.pause(cx, 200).await;
    }

    // --- the file in front ----------------------------------------------

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn editor(&self, cx: &mut Cx) -> Option<Entity<FileEditor>> {
        self.app(cx, |_, cx| context::focused_file(cx)?.editor.upgrade())
            .flatten()
    }

    fn input<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut Input, &mut Context<Input>) -> R,
    ) -> Option<R> {
        let editor = self.editor(cx)?;
        self.app(cx, |_, cx| {
            let input = editor.read(cx).input.clone();
            input.update(cx, f)
        })
    }

    /// The text of the file in front.
    fn text(&self, cx: &mut Cx) -> String {
        self.input(cx, |input, _| input.content.to_string())
            .unwrap_or_default()
    }

    /// The path of the file in front, and the caret's line and column from 1.
    fn place(&self, cx: &mut Cx) -> Option<(String, usize, usize)> {
        let path = self.app(cx, |_, cx| context::focused_file(cx).map(|file| file.path))??;
        let (line, column) = self.input(cx, |input, _| input.cursor_location())?;
        Some((path, line, column))
    }

    /// Opens a file of the sample project and waits for its text.
    async fn open(&self, cx: &mut Cx, name: &str) -> bool {
        let path = self.path(name).to_string_lossy().into_owned();
        let opened = path.clone();
        if self.app(cx, move |window, cx| context::open_file(&opened, None, window, cx))
            != Some(true)
        {
            return false;
        }
        self.until(cx, 8000, |_, cx| {
            context::focused_file(cx).is_some_and(|file| {
                file.path == path
                    && (file.editor.upgrade())
                        .is_some_and(|editor| !editor.read(cx).input.read(cx).content.is_empty())
            })
        })
        .await
    }

    /// The offset of `needle` in the file in front and the window position
    /// of its first character. The sample's files fit in the pane.
    fn glyph(&self, cx: &mut Cx, needle: &str) -> Option<(usize, Point<Pixels>)> {
        let offset = self.text(cx).find(needle)?;
        let position = self.input(cx, |input, _| input.glyph_center(offset))??;
        Some((offset, position))
    }

    /// The zero-based row of the first line holding `needle`.
    fn row(&self, cx: &mut Cx, needle: &str) -> Option<usize> {
        let text = self.text(cx);
        Some(code_editor::row_of(&text, text.find(needle)?))
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
