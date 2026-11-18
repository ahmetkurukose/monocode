//! Steps 9 and 10: a breakpoint set in the gutter, the script debugged from
//! the header to its end, and one test debugged from its marker's menu.
use super::{
    running::{SCRIPT, TESTS},
    *,
};
use crate::{code_editor::GutterSlot, debug_ui::probe};

impl Driver {
    pub(super) async fn debugging(&mut self, cx: &mut Cx) {
        self.debug_script(cx).await;
        self.debug_test(cx).await;
    }

    pub(super) fn debug_sessions(&self, cx: &mut Cx) -> Vec<Value> {
        self.app(cx, |_, cx| probe::sessions(cx)).unwrap_or_default()
    }

    fn session_ids(&self, cx: &mut Cx) -> Vec<String> {
        (self.debug_sessions(cx).iter())
            .filter_map(|session| session["id"].as_str().map(str::to_owned))
            .collect()
    }

    /// The newest session that is not one of `known`, once `done` holds for
    /// it; what it is when time runs out otherwise.
    pub(super) async fn session_when(
        &self,
        cx: &mut Cx,
        millis: u64,
        known: &[String],
        done: impl Fn(&Value) -> bool,
    ) -> Option<Value> {
        let newest = |sessions: Vec<Value>| {
            (sessions.into_iter().rev())
                .find(|session| !known.iter().any(|id| session["id"].as_str() == Some(id)))
        };
        for _ in 0..=millis / 100 {
            if let Some(session) = newest(self.debug_sessions(cx))
                && done(&session)
            {
                // The rows are filled a moment after the state changes.
                self.pause(cx, 400).await;
                return newest(self.debug_sessions(cx));
            }
            self.pause(cx, 100).await;
        }
        newest(self.debug_sessions(cx))
    }

    /// The decorated rows of a file as `[row, style, arrow]`.
    fn decorations(&self, cx: &mut Cx, path: &Path) -> Vec<Value> {
        self.app(cx, |_, cx| {
            (code_editor::hub(cx).read(cx).line_decorations(path).iter())
                .map(|line| json!([line.row, format!("{:?}", line.style), line.arrow]))
                .collect()
        })
        .unwrap_or_default()
    }

    /// Clicks the breakpoint column on the row holding `needle` in the file
    /// in front; the file's breakpoint lines afterwards.
    async fn toggle_breakpoint(&self, cx: &mut Cx, name: &str, needle: &str) -> Vec<u32> {
        let row = self.row(cx, needle);
        let mut at = None;
        for _ in 0..30 {
            at = row.and_then(|row| {
                self.input(cx, |input, _| {
                    input.gutter_slot_center(GutterSlot::Breakpoint, row)
                })
                .flatten()
            });
            if at.is_some() {
                break;
            }
            self.pause(cx, 100).await;
        }
        if let Some(at) = at {
            self.click(cx, at).await;
            self.pause(cx, 300).await;
        }
        probe::breakpoint_lines(&self.path(name))
    }

    async fn debug_script(&mut self, cx: &mut Cx) {
        let path = self.path(SCRIPT);
        let opened = self.open(cx, SCRIPT).await;
        self.calm(cx).await;
        let line = self.row(cx, "print(\"order total\"").map_or(0, |row| row as u32 + 1);
        let set = self.toggle_breakpoint(cx, SCRIPT, "print(\"order total\"").await;
        self.shot(cx, "breakpoint-in-gutter").await;

        let known = self.session_ids(cx);
        let clicked = self.click_on(cx, "file-header-debug-file").await;
        let paused = |session: &Value| session["state"] == "paused";
        let stopped = self.session_when(cx, 60_000, &known, paused).await;
        // The variables are asked for once the stop is shown.
        self.until(cx, 8000, |_, cx| {
            (probe::sessions(cx).last())
                .is_some_and(|session| !session["variables"].as_array().is_some_and(Vec::is_empty))
        })
        .await;
        let stopped = match &stopped {
            Some(_) => self.session_when(cx, 0, &known, |_| true).await,
            None => None,
        };
        let on_line = |session: &Option<Value>, line: u32| {
            session.as_ref().is_some_and(|session| {
                session["state"] == "paused"
                    && session["stop"]["line"] == json!(line)
                    && session["stop"]["path"].as_str() == path.to_str()
            })
        };
        let at_breakpoint = on_line(&stopped, line);
        let arrow = self.decorations(cx, &path);
        let arrow_ok = arrow == vec![json!([line - 1, "Execution", true])];
        let has = |session: &Option<Value>, key: &str, wanted: Value| {
            session.as_ref().is_some_and(|session| {
                (session[key].as_array()).is_some_and(|rows| rows.contains(&wanted))
            })
        };
        let frame_ok = stopped.as_ref().is_some_and(|session| {
            (session["stack"].as_array()).is_some_and(|frames| {
                frames.first().is_some_and(|frame| {
                    frame["name"] == "summarize" && frame["line"] == json!(line)
                })
            })
        });
        let locals_ok = has(&stopped, "variables", json!(["total", "42.0", 1]))
            && has(&stopped, "variables", json!(["count", "3", 1]));
        let drawn: Vec<(&str, bool)> = [
            "debug-tab",
            "debug-controls",
            "debug-stack",
            "debug-variables",
            "debug-console",
        ]
        .into_iter()
        .map(|selector| (selector, self.bounds(cx, selector).is_some()))
        .collect();
        let tab_ok = drawn.iter().all(|(_, drawn)| *drawn)
            && stopped.as_ref().is_some_and(|session| session["tab_open"] == true);

        // An expression typed into the console.
        let typed = self.click_on(cx, "debug-console-input").await;
        self.type_text(cx, "total + 1").await;
        self.keys(cx, "enter").await;
        let answered = |session: &Value| {
            (session["console"].as_array())
                .is_some_and(|lines| lines.contains(&json!(["Result", "43.0"])))
        };
        let evaluated = self.session_when(cx, 8000, &known, answered).await;
        let evaluate_ok = evaluated.as_ref().is_some_and(answered);
        self.shot(cx, "debug-stopped-at-breakpoint").await;

        // Step over, with the button.
        let stepped_click = self.click_on(cx, "debug-step-over").await;
        let next = |session: &Value| {
            session["state"] == "paused" && session["stop"]["line"] == json!(line + 1)
        };
        let stepped = self.session_when(cx, 15_000, &known, next).await;
        let step_ok = on_line(&stepped, line + 1)
            && self.decorations(cx, &path) == vec![json!([line, "Execution", true])];
        self.shot(cx, "debug-after-step-over").await;

        // Continue, from the keyboard, to the end.
        self.keys(cx, "f5").await;
        let ended = |session: &Value| session["state"] == "ended";
        let finished = self.session_when(cx, 30_000, &known, ended).await;
        let exit_ok = finished
            .as_ref()
            .is_some_and(|session| session["state"] == "ended" && session["exit_code"] == 0);
        let output = finished
            .as_ref()
            .and_then(|session| session["terminal"]["tail"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let output_ok = output.contains("order total 42.0") && output.contains("done 42.0");
        self.pause(cx, 500).await;
        let cleared = self.decorations(cx, &path);
        self.shot(cx, "debug-ended").await;

        let short = |session: &Option<Value>| {
            session.as_ref().map(|session| {
                json!({
                    "title": session["title"], "state": session["state"],
                    "stop": session["stop"], "exit_code": session["exit_code"],
                    "failure": session["failure"], "notice": session["notice"],
                    "stack": session["stack"], "scopes": session["scopes"],
                    "variables": session["variables"], "console": session["console"],
                })
            })
        };
        self.record(
            "9",
            "a breakpoint set in the gutter stops the script debugged from the header: arrow, stack, locals, console, step over, continue to exit 0, output in its terminal tab, decorations cleared",
            opened && set == [line] && clicked && at_breakpoint && arrow_ok && frame_ok
                && locals_ok && tab_ok && evaluate_ok && step_ok && exit_ok && output_ok
                && cleared.is_empty(),
            json!({
                "breakpoint_lines_after_click": set, "expected_line": line,
                "debug_button_clicked": clicked,
                "checks": {
                    "stopped_on_breakpoint": at_breakpoint, "arrow": arrow_ok,
                    "frame": frame_ok, "locals": locals_ok, "tab": tab_ok,
                    "console_input_clicked": typed, "evaluate": evaluate_ok,
                    "step_button_clicked": stepped_click, "step_over": step_ok,
                    "exit_0": exit_ok, "output_in_terminal": output_ok,
                    "decorations_cleared": cleared.is_empty(),
                },
                "decorations_at_stop": arrow, "drawn": drawn,
                "at_stop": short(&evaluated.or(stopped)), "after_step": short(&stepped),
                "at_end": short(&finished), "terminal_tail": output,
                "decorations_at_end": cleared,
                "notice": self.app(cx, |_, cx| crate::run_ui::probe::last_notice(cx)),
            }),
        );
        // The breakpoint is taken away again, by the same click.
        self.open(cx, SCRIPT).await;
        self.toggle_breakpoint(cx, SCRIPT, "print(\"order total\"").await;
    }

    async fn debug_test(&mut self, cx: &mut Cx) {
        let path = self.path(TESTS);
        let opened = self.open(cx, TESTS).await;
        let needle = "discounted = apply_discount";
        let line = self.row(cx, needle).map_or(0, |row| row as u32 + 1);
        let set = self.toggle_breakpoint(cx, TESTS, needle).await;

        // The marker's menu, by a right press; its "Debug" row, by a click.
        let known = self.session_ids(cx);
        let row = self.row(cx, "def test_discount_is_wrong");
        let marker = row.and_then(|row| {
            self.input(cx, |input, _| input.gutter_slot_center(GutterSlot::Run, row))
                .flatten()
        });
        if let Some(marker) = marker {
            self.press(cx, marker, MouseButton::Right, Modifiers::default())
                .await;
        }
        self.pause(cx, 500).await;
        self.shot(cx, "gutter-marker-menu").await;
        let menu_row = self.click_on(cx, "menu-action-debug:0").await;
        let paused = |session: &Value| session["state"] == "paused";
        self.session_when(cx, 90_000, &known, paused).await;
        self.until(cx, 8000, |_, cx| {
            (probe::sessions(cx).last())
                .is_some_and(|session| !session["variables"].as_array().is_some_and(Vec::is_empty))
        })
        .await;
        let stopped = self.session_when(cx, 0, &known, |_| true).await;
        self.shot(cx, "debug-stopped-in-test").await;
        let inside = stopped.as_ref().is_some_and(|session| {
            session["state"] == "paused"
                && session["stop"]["line"] == json!(line)
                && session["stop"]["path"].as_str() == path.to_str()
                && session["stack"][0]["name"] == "test_discount_is_wrong"
        });
        let has_order = stopped.as_ref().is_some_and(|session| {
            (session["variables"].as_array())
                .is_some_and(|rows| rows.iter().any(|row| row[0] == "order"))
        });
        // Stop, with the button.
        let stop_clicked = self.click_on(cx, "debug-stop").await;
        let ended = |session: &Value| session["state"] == "ended";
        let finished = self.session_when(cx, 20_000, &known, ended).await;
        let ended_ok = finished.as_ref().is_some_and(ended);
        self.pause(cx, 500).await;
        let cleared = self.decorations(cx, &path);
        self.record(
            "10",
            "one test debugged from its gutter marker's menu stops on a breakpoint inside it, and Stop ends it",
            opened && set == [line] && marker.is_some() && menu_row && inside && has_order
                && stop_clicked && ended_ok && cleared.is_empty(),
            json!({
                "breakpoint_lines": set, "expected_line": line,
                "marker_drawn": marker.is_some(), "debug_row_clicked": menu_row,
                "stopped_inside": inside,
                "at_stop": stopped.as_ref().map(|session| json!({
                    "title": session["title"], "state": session["state"],
                    "stop": session["stop"], "stack": session["stack"],
                    "variables": session["variables"], "failure": session["failure"],
                    "notice": session["notice"],
                })),
                "stop_clicked": stop_clicked,
                "ended": finished.as_ref().map(|session| session["state"].clone()),
                "decorations_at_end": cleared,
                "notice": self.app(cx, |_, cx| crate::run_ui::probe::last_notice(cx)),
            }),
        );
        self.open(cx, TESTS).await;
        self.toggle_breakpoint(cx, TESTS, needle).await;
    }
}
