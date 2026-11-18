//! Step 11's app side and step 13. A held app writes the environment a
//! session's agent process would be given, so that `scripts/code-selftest/
//! agent.py` can start `monocode-gpui code-mcp` with it and use the tools as
//! a provider does; the script then asks this loop, through files in the
//! hold folder, what the app shows of what the agent did, and for the click
//! on Take over. Step 13 sets the idle time to one minute and closes the
//! file panes.
use super::{insight::ORDERS, running::SCRIPT, *};
use crate::code_shell::rail;
use monocode_core::code::services::services;

impl Driver {
    /// The tool environment of the session, then answers until told to quit.
    pub(super) async fn hold(&mut self, cx: &mut Cx) {
        let Some(folder) = std::env::var_os("MONOCODE_CODE_SELFTEST_HOLD").map(PathBuf::from)
        else {
            return;
        };
        self.open(cx, SCRIPT).await;
        self.calm(cx).await;
        let session = self
            .shell(cx, |shell, _, _| {
                shell
                    .terminal_owner()
                    .or_else(|| shell.project_session_for_terminal())
            })
            .flatten();
        let environment = match (session, std::env::current_exe()) {
            (Some(session), Ok(executable)) => monocode_core::code::bridge::environment(
                session,
                &self.root,
                &self.root,
                &executable,
            )
            .map(|environment| json!({ "environment": environment, "session": session })),
            _ => Err("no session to act for".to_owned()),
        };
        let told = environment.unwrap_or_else(|error| json!({ "error": error }));
        let _ = std::fs::write(folder.join("env.json"), told.to_string());
        let (ask, answer) = (folder.join("ask.json"), folder.join("answer.json"));
        loop {
            self.pause(cx, 200).await;
            let Ok(text) = std::fs::read_to_string(&ask) else {
                continue;
            };
            let Ok(command) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let _ = std::fs::remove_file(&ask);
            let name = command["cmd"].as_str().unwrap_or_default().to_owned();
            if name == "quit" {
                let _ = std::fs::write(&answer, json!({ "id": command["id"] }).to_string());
                return;
            }
            let result = self.answer(cx, &name, &command, session).await;
            let _ = std::fs::write(
                &answer,
                json!({ "id": command["id"], "result": result }).to_string(),
            );
        }
    }

    async fn answer(
        &mut self,
        cx: &mut Cx,
        name: &str,
        command: &Value,
        session: Option<i64>,
    ) -> Value {
        match name {
            "state" => self.app_state(cx, session),
            "take_over" => {
                let clicked = self.click_on(cx, "debug-take-over").await;
                self.pause(cx, 500).await;
                let mut state = self.app_state(cx, session);
                state["clicked"] = json!(clicked);
                state
            }
            "shot" => {
                let label = command["name"].as_str().unwrap_or("agent").to_owned();
                self.shot(cx, &label).await;
                json!({ "pictures": self.pictures.len() })
            }
            _ => json!({ "error": format!("unknown command {name}") }),
        }
    }

    /// What the app shows of the project's debug sessions, runs and servers.
    fn app_state(&self, cx: &mut Cx, session: Option<i64>) -> Value {
        let status = self
            .app(cx, move |_, cx| {
                session.and_then(|session| rail::status(session, cx).map(|text| text.to_string()))
            })
            .flatten();
        let runs: Vec<Value> = (self.runs().iter())
            .map(|info| {
                json!({
                    "id": format!("{:?}", info.id), "label": info.label,
                    "state": format!("{:?}", info.state), "by": format!("{:?}", info.started_by),
                })
            })
            .collect();
        json!({
            "rail_status": status,
            "sessions": self.debug_sessions(cx),
            "runs": runs,
            "debug_tab_drawn": self.bounds(cx, "debug-tab").is_some(),
            "take_over_drawn": self.bounds(cx, "debug-take-over").is_some(),
        })
    }

    /// Step 13: with the idle time at its smallest, closing the file panes
    /// leaves the language servers unused, and they are stopped.
    pub(super) async fn idle(&mut self, cx: &mut Cx) {
        let project = self.root.to_string_lossy().into_owned();
        self.open(cx, ORDERS).await;
        let before = self.server_states();
        let set = {
            let project = project.clone();
            self.app(cx, move |window, cx| {
                crate::code_insight::probe::set_idle_minutes(&project, 1, window, cx)
            })
        };
        // Saving the setting reopens the project's documents under it.
        self.pause(cx, 3000).await;
        let reopened = self.open(cx, ORDERS).await;
        let alive_before = (self.server_states().iter())
            .filter(|(_, state)| state == "Ready" || state == "Starting")
            .count();
        // The panes of files, closed as the user's Cmd+W does.
        let mut closed = 0;
        for _ in 0..12 {
            if self.app(cx, |_, cx| context::focused_file(cx).is_none()) != Some(false) {
                break;
            }
            self.keys(cx, "cmd-w").await;
            self.pause(cx, 400).await;
            closed += 1;
        }
        let files_left = self
            .app(cx, |_, cx| context::focused_file(cx).is_some())
            .unwrap_or(true);
        let started = std::time::Instant::now();
        let mut stopped_after = None;
        while started.elapsed() < Duration::from_secs(150) {
            let servers = self.server_states();
            if !servers.is_empty()
                && (servers.iter()).all(|(_, state)| state != "Ready" && state != "Starting")
                || servers.is_empty() && alive_before > 0
            {
                stopped_after = Some(started.elapsed().as_secs());
                break;
            }
            self.pause(cx, 1000).await;
        }
        let after = self.server_states();
        let processes = pgrep_sample(&self.root);
        self.record(
            "13",
            "with the idle time set to one minute, closing the file panes stops the language servers",
            set == Some(true) && reopened && alive_before >= 2 && !files_left
                && stopped_after.is_some() && processes.is_empty(),
            json!({
                "servers_before": before, "idle_setting_saved": set,
                "alive_before_close": alive_before, "panes_closed_with_cmd_w": closed,
                "file_panes_left": files_left, "stopped_after_seconds": stopped_after,
                "servers_after": after, "server_processes_after": processes,
            }),
        );
    }

    fn server_states(&self) -> Vec<(String, String)> {
        (services().lsp().statuses().into_iter())
            .filter(|status| status.root.starts_with(&self.root))
            .map(|status| (status.name, format!("{:?}", status.state)))
            .collect()
    }
}

/// Command lines of processes under the sample project that are language
/// servers, from `ps`.
fn pgrep_sample(root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let marks = [
        format!("{}/.venv/bin/basedpyright", root.display()),
        format!("{}/.venv/bin/ruff", root.display()),
    ];
    (String::from_utf8_lossy(&output.stdout).lines())
        .filter(|line| marks.iter().any(|mark| line.contains(mark.as_str())))
        .map(|line| line.trim().chars().take(200).collect())
        .collect()
}
