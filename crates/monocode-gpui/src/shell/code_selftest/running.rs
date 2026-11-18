//! Steps 7 and 8: the header's Run button, running again, a test from its
//! gutter marker, and a configuration of the project's own from the
//! header's menu and from quick open.
use super::*;
use crate::{
    code_editor::GutterSlot,
    code_shell::{chips, settings},
    run_ui::probe,
};
use monocode_core::code::{
    run::{RunInfo, RunState},
    services::services,
};

pub(super) const SCRIPT: &str = "main.py";
pub(super) const TESTS: &str = "tests/test_orders.py";

impl Driver {
    pub(super) async fn running(&mut self, cx: &mut Cx) {
        self.run_script(cx).await;
        self.run_test(cx).await;
        self.run_configuration(cx).await;
    }

    /// The registry's runs of the sample project, oldest first.
    pub(super) fn runs(&self) -> Vec<RunInfo> {
        let mut runs: Vec<RunInfo> = (services().runs().list().into_iter())
            .filter(|info| info.checkout == self.root)
            .collect();
        runs.sort_by_key(|info| info.id);
        runs
    }

    /// The run that follows the `known` ones, once it has ended.
    async fn next_run(&self, cx: &mut Cx, known: usize, millis: u64) -> Option<RunInfo> {
        for _ in 0..=millis / 100 {
            if let Some(info) = self.runs().into_iter().nth(known)
                && !info.state.is_running()
            {
                // The tail is fed from the terminal a moment behind the exit.
                self.pause(cx, 300).await;
                return self.runs().into_iter().nth(known);
            }
            self.pause(cx, 100).await;
        }
        self.runs().into_iter().nth(known)
    }

    fn output(&self, info: &RunInfo) -> String {
        (services().runs().output(info.id, 0)).map_or(String::new(), |output| output.text)
    }

    /// A run as the report shows it.
    fn told(&self, info: &RunInfo, cx: &mut Cx) -> Value {
        let (tab, row) = self
            .app(cx, |_, cx| {
                (probe::has_tab(info.id, cx), probe::chip_row(info, cx))
            })
            .unwrap_or_default();
        let output = self.output(info);
        let tail: String = output.chars().rev().take(600).collect::<String>();
        json!({
            "label": info.label, "command": info.spawn.command, "args": info.spawn.args,
            "state": format!("{:?}", info.state), "has_tab": tab, "chip_row": row,
            "output_tail": tail.chars().rev().collect::<String>(),
        })
    }

    /// The running-work chip as `text (tone)`, or `hidden`.
    fn running_chip(&self, cx: &mut Cx) -> String {
        self.app(cx, |_, cx| {
            chips::find("code-running", cx).and_then(|chip| (chip.state)(cx))
        })
        .flatten()
        .map_or("hidden".to_owned(), |state| {
            format!("{} ({:?})", state.text, state.tone)
        })
    }

    async fn run_script(&mut self, cx: &mut Cx) {
        let opened = self.open(cx, SCRIPT).await;
        self.calm(cx).await;
        let known = self.runs().len();
        let clicked = self.click_on(cx, "file-header-run-file").await;
        let first = self.next_run(cx, known, 30_000).await;
        self.shot(cx, "run-tab-script").await;
        let ok = |info: &RunInfo, output: &str| {
            info.state == RunState::Exited(Some(0))
                && output.contains("order total 42.0")
                && output.contains("done 42.0")
        };
        let first_ok = first.as_ref().is_some_and(|info| ok(info, &self.output(info)));
        let tab = first
            .as_ref()
            .and_then(|info| self.app(cx, |_, cx| probe::has_tab(info.id, cx)))
            .unwrap_or(false);
        let dock_drawn = self.bounds(cx, "dock-run-controls").is_some()
            && self.bounds(cx, "run-status-dot").is_some();
        let first_told = first.as_ref().map(|info| self.told(info, cx));

        // Run again, from the keyboard.
        self.keys(cx, "ctrl-alt-r").await;
        let again = self.next_run(cx, known + 1, 30_000).await;
        let again_ok = again.as_ref().is_some_and(|info| ok(info, &self.output(info)));
        let again_told = again.as_ref().map(|info| self.told(info, cx));
        let chip = self.running_chip(cx);
        self.record(
            "7a",
            "the header's Run button runs the script in a dock tab: its output, exit 0, a registry entry; Ctrl+Alt+R runs it again",
            opened && clicked && first_ok && tab && dock_drawn && again_ok,
            json!({
                "button_clicked": clicked, "run": first_told, "tab_controls_drawn": dock_drawn,
                "rerun": again_told, "notice": self.app(cx, |_, cx| probe::last_notice(cx)),
                "running_chip_afterwards": chip,
            }),
        );
    }

    async fn run_test(&mut self, cx: &mut Cx) {
        let opened = self.open(cx, TESTS).await;
        let row = self.row(cx, "def test_discount_is_wrong");
        // The markers come from a search off the UI thread.
        let mut marker = None;
        for _ in 0..50 {
            marker = row.and_then(|row| {
                self.input(cx, |input, _| input.gutter_slot_center(GutterSlot::Run, row))
                    .flatten()
            });
            if marker.is_some() {
                break;
            }
            self.pause(cx, 100).await;
        }
        let known = self.runs().len();
        self.shot(cx, "test-file-gutter-markers").await;
        if let Some(marker) = marker {
            self.click(cx, marker).await;
        }
        let run = self.next_run(cx, known, 60_000).await;
        self.shot(cx, "run-tab-failing-test").await;
        let told = run.as_ref().map(|info| self.told(info, cx));
        let one_test = run.as_ref().is_some_and(|info| {
            (info.spawn.args.iter())
                .any(|arg| arg.ends_with("test_orders.py::TestOrders::test_discount_is_wrong"))
        });
        let failed = run.as_ref().is_some_and(|info| {
            info.state == RunState::Exited(Some(1)) && self.output(info).contains("1 failed")
        });
        let shown = told
            .as_ref()
            .is_some_and(|told| told["chip_row"][1].as_str().is_some_and(|d| d.contains("Exit 1")));
        // The list of the footer's chip, where a finished run stays reachable.
        let chip = self.running_chip(cx);
        let chip_clicked = self.click_on(cx, "code-running").await;
        self.shot(cx, "running-chip-list").await;
        self.keys(cx, "escape").await;
        self.record(
            "7b",
            "the gutter marker of the failing test runs that one test under pytest and exit 1 is shown",
            opened && marker.is_some() && one_test && failed && shown,
            json!({
                "marker_row": row, "marker_drawn": marker.is_some(), "run": told,
                "running_chip": chip, "chip_list_opened": chip_clicked,
            }),
        );
    }

    async fn run_configuration(&mut self, cx: &mut Cx) {
        let project = self.root.to_string_lossy().into_owned();
        let added = {
            let project = project.clone();
            self.app(cx, move |window, cx| {
                probe::add_configuration(&project, "Pytest quiet", "$PYTHON -m pytest -q", window, cx)
            })
            .flatten()
        };
        self.pause(cx, 300).await;
        let saved = {
            let project = project.clone();
            self.app(cx, move |_, cx| {
                (settings::runs(&project, cx).configs.iter())
                    .map(|config| (config.name.clone(), config.command.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        };
        self.open(cx, SCRIPT).await;
        self.calm(cx).await;
        let (path, root) = (self.path(SCRIPT), self.root.clone());
        let menu = self
            .app(cx, move |_, cx| probe::header_menu(&path, &root, cx))
            .unwrap_or_default();
        let in_menu = menu.iter().any(|(_, label, _)| label == "Pytest quiet");

        // From the header's menu, with the mouse.
        let known = self.runs().len();
        let dropdown = self.click_on(cx, "file-header-run-file-menu").await;
        self.pause(cx, 300).await;
        self.shot(cx, "header-run-menu").await;
        let row = added.as_ref().map(|id| format!("menu-action-run:{id}"));
        let picked = match &row {
            Some(row) => self.click_on(cx, row).await,
            None => false,
        };
        let from_menu = self.next_run(cx, known, 90_000).await;
        let ran = |info: &RunInfo, output: &str| {
            info.label == "Pytest quiet"
                && info.state == RunState::Exited(Some(1))
                && output.contains("1 failed, 2 passed")
        };
        let menu_ok = from_menu.as_ref().is_some_and(|info| ran(info, &self.output(info)));
        let menu_told = from_menu.as_ref().map(|info| self.told(info, cx));
        self.keys(cx, "escape").await;

        // From quick open's commands, typed.
        let known = self.runs().len();
        self.keys(cx, "cmd-shift-p").await;
        self.pause(cx, 300).await;
        self.type_text(cx, "Run: Pytest").await;
        self.pause(cx, 500).await;
        let rows = self
            .shell(cx, |shell, _, cx| {
                (shell.quick_open.as_ref())
                    .map(|open| open.picker().read(cx).row_labels())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.shot(cx, "quick-open-run-commands").await;
        let listed = rows.first().is_some_and(|(title, _)| title == "Run: Pytest quiet");
        self.keys(cx, "enter").await;
        let from_palette = self.next_run(cx, known, 90_000).await;
        let palette_ok = from_palette
            .as_ref()
            .is_some_and(|info| ran(info, &self.output(info)));
        let palette_told = from_palette.as_ref().map(|info| self.told(info, cx));
        self.keys(cx, "escape").await;

        // The page itself, as the project menu opens it.
        let page = {
            let project = project.clone();
            self.app(cx, move |window, cx| {
                settings::open(&project, "run-configurations", window, cx)
            })
            .unwrap_or(false)
        };
        self.pause(cx, 600).await;
        let page_drawn = self.bounds(cx, "run-configurations-page").is_some();
        self.shot(cx, "run-configurations-page").await;
        let focus_in_dialog = self.shell(cx, |shell, window, cx| {
            shell.background_dialog.is_some()
                && shell.code_page_dialog().is_some_and(|dialog| {
                    use gpui::Focusable;
                    dialog.focus_handle(cx).contains_focused(window, cx)
                })
        });
        self.keys(cx, "escape").await;
        self.pause(cx, 300).await;
        let mut escapes = 1;
        while self.bounds(cx, "run-configurations-page").is_some() && escapes < 4 {
            self.keys(cx, "escape").await;
            self.pause(cx, 300).await;
            escapes += 1;
        }
        let page_closed = self.bounds(cx, "run-configurations-page").is_none();
        self.record(
            "8",
            "a configuration made through the settings page is in the header's menu and in quick open, and runs from both",
            added.is_some() && in_menu && dropdown && picked && menu_ok && listed && palette_ok
                && page && page_drawn,
            json!({
                "saved": saved, "header_menu": menu, "dropdown_clicked": dropdown,
                "menu_row_clicked": picked, "run_from_menu": menu_told,
                "quick_open_rows": rows, "run_from_quick_open": palette_told,
                "page_opened": page, "page_drawn": page_drawn,
                "page_closed_by_escape": page_closed, "focus_in_dialog": focus_in_dialog, "escapes_needed": escapes,
            }),
        );
    }
}
