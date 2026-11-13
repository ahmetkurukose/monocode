//! The browser pane checked from the inside, in a build nobody is using.
//!
//! A native web view cannot be driven by GPUI's test harness: what matters
//! (does a click give the page the keyboard, do keys reach it, does paste
//! work) only happens in a real window. With `MONOCODE_BROWSER_SELFTEST` set
//! to a file, the app opens `MONOCODE_BROWSER_SELFTEST_URL` in a browser
//! pane, sends real mouse and key events through its own event path, writes
//! what the page then holds to that file, and quits. Run it against a
//! throwaway data directory (`MONOCODE_NATIVE_DATA_DIR`), never the user's.
use super::*;
use std::time::Duration;

type Step = (String, serde_json::Value);

async fn wait(cx: &mut gpui::AsyncWindowContext, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

impl Shell {
    pub(super) fn start_browser_selftest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The agent side: write down what a session's agent process would be
        // given, and stay open, so that a script can drive the tools the way
        // a provider does (`monocode-gpui browser-mcp` with that environment).
        if let Some(out) = std::env::var_os("MONOCODE_BROWSER_AGENT_TEST") {
            cx.spawn_in(window, async move |this, cx| {
                wait(cx, 1500).await;
                let _ = cx.update(|window, cx| {
                    cx.activate(true);
                    window.activate_window();
                });
                wait(cx, 800).await;
                let _ = this.update_in(cx, |this, _, cx| {
                    if this.terminal_owner().is_none()
                        && this.project_session_for_terminal().is_none()
                    {
                        this.new_chat(cx);
                    }
                });
                wait(cx, 800).await;
                let session = this
                    .update_in(cx, |this, _, _| {
                        this.terminal_owner()
                            .or_else(|| this.project_session_for_terminal())
                    })
                    .ok()
                    .flatten();
                let report = match session.zip(std::env::current_exe().ok()) {
                    Some((session, executable)) => {
                        match monocode_core::browser_bridge::environment(session, &executable) {
                            Ok(environment) => serde_json::json!({"environment": environment}),
                            Err(error) => serde_json::json!({"error": error}),
                        }
                    }
                    None => serde_json::json!({"error": "no session to act for"}),
                };
                let _ = std::fs::write(&out, report.to_string());
            })
            .detach();
            return;
        }
        let Some(out) = std::env::var_os("MONOCODE_BROWSER_SELFTEST") else {
            return;
        };
        let url = std::env::var("MONOCODE_BROWSER_SELFTEST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8765/".into());
        cx.spawn_in(window, async move |this, cx| {
            let mut steps: Vec<Step> = Vec::new();
            let outcome = Self::run_browser_selftest(&this, cx, &url, &mut steps).await;
            if let Err(error) = outcome {
                steps.push(("error".into(), error.into()));
            }
            let report: serde_json::Map<String, serde_json::Value> = steps.into_iter().collect();
            let _ = std::fs::write(
                &out,
                serde_json::to_string_pretty(&report).unwrap_or_default(),
            );
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
    }

    async fn run_browser_selftest(
        this: &gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncWindowContext,
        url: &str,
        steps: &mut Vec<Step>,
    ) -> Result<(), String> {
        let gone = |_| "The window closed".to_string();
        wait(cx, 1500).await;
        // A window behind others is not drawn, and an app that is not in
        // front has no key window to send keys to.
        cx.update(|window, cx| {
            cx.activate(true);
            window.activate_window();
        })
        .map_err(gone)?;
        wait(cx, 1000).await;
        let url = url.to_owned();
        let pane = this
            .update_in(cx, |this, window, cx| {
                if this.terminal_owner().is_none() && this.project_session_for_terminal().is_none()
                {
                    this.new_chat(cx);
                }
                this.new_browser_pane(Some(url), window, cx);
                this.workspace.active
            })
            .map_err(gone)?
            .filter(|pane| *pane < 0)
            .ok_or("No browser pane was opened")?;
        let page = |this: &mut Self| this.browser_views.get(&pane).cloned();
        // Loaded: the page says so, and has had a moment to run its script.
        let mut loaded = false;
        for _ in 0..40 {
            wait(cx, 250).await;
            let state = this
                .update_in(cx, |this, _, cx| {
                    page(this).map(|page| {
                        let page = page.read(cx);
                        (page.loading(), page.title.clone(), page.url.clone())
                    })
                })
                .map_err(gone)?
                .ok_or("The pane has no page")?;
            if !state.0 && !state.1.is_empty() {
                steps.push(("title".into(), state.1.into()));
                steps.push(("url".into(), state.2.into()));
                loaded = true;
                break;
            }
        }
        steps.push(("loaded".into(), loaded.into()));
        let diagnostics = this
            .update_in(cx, |this, window, cx| {
                let size = window.viewport_size();
                serde_json::json!({
                    "render_passes": this.render_passes,
                    "window_active": window.is_window_active(),
                    "viewport": [size.width.as_f32(), size.height.as_f32()],
                    "layout_leaves": this.workspace.layout(pane).leaf_ids(),
                    "mono_active": this.mono_active(cx),
                    "search": this.search_page(),
                    "page": page(this).map(|page| page.read(cx).diagnostics()),
                    "active": this.workspace.active,
                    "tab": this.workspace.tab_of(pane),
                    "app_view": this.app_view_open(),
                    "settings": this.settings.is_some(),
                    "classic": this.workspace.classic_layout,
                })
            })
            .map_err(gone)?;
        steps.push(("state".into(), diagnostics));
        if !loaded {
            return Err("The page did not finish loading".into());
        }
        macro_rules! eval {
            ($script:expr) => {{
                let receive = this
                    .update_in(cx, |this, _, cx| {
                        page(this).map(|page| page.read(cx).eval($script))
                    })
                    .map_err(gone)?
                    .ok_or("The pane has no page")?;
                let text = receive.recv().await.map_err(|e| e.to_string())??;
                serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())?
            }};
        }
        macro_rules! act {
            ($action:expr) => {{
                let event: Option<Box<dyn FnOnce()>> = this
                    .update_in(cx, |this, _, cx| {
                        page(this).map(|page| {
                            let page = page.read(cx);
                            #[allow(clippy::redundant_closure_call)]
                            ($action)(page)
                        })
                    })
                    .map_err(gone)?;
                // Sent here, with no GPUI update on the stack.
                if let Some(event) = event {
                    event();
                }
                wait(cx, 350).await;
            }};
        }
        macro_rules! keyboard {
            ($name:expr) => {{
                let has = this
                    .update_in(cx, |this, _, cx| {
                        page(this).is_some_and(|page| page.read(cx).has_keyboard())
                    })
                    .map_err(gone)?;
                steps.push(($name.into(), has.into()));
            }};
        }
        let centre = |rect: &serde_json::Value| {
            (
                rect["x"].as_f64().unwrap_or(0.) + rect["w"].as_f64().unwrap_or(0.) / 2.,
                rect["y"].as_f64().unwrap_or(0.) + rect["h"].as_f64().unwrap_or(0.) / 2.,
            )
        };
        const RECT: &str = "(id=>{const r=document.getElementById(id).getBoundingClientRect();\
                            return {x:r.x,y:r.y,w:r.width,h:r.height}})";
        steps.push(("user_agent".into(), eval!("navigator.userAgent")));

        // A click in a field gives the page the keyboard and the field the caret.
        let (x, y) = centre(&eval!(&format!("{RECT}('a')")));
        macro_rules! probe {
            ($name:expr) => {{
                let probe = this
                    .update_in(cx, |this, _, cx| {
                        page(this).map(|page| page.read(cx).probe(x, y))
                    })
                    .map_err(gone)?;
                steps.push(($name.into(), probe.unwrap_or_default()));
            }};
        }
        probe!("probe_before_click");
        act!(|page: &crate::browser::BrowserView| page.click_at(x, y));
        probe!("probe_after_click");
        steps.push((
            "focused_after_click".into(),
            eval!("document.activeElement.id"),
        ));
        keyboard!("keyboard_after_click");

        // Typing reaches it, also after the app has drawn more frames.
        for (letter, code) in [("h", 4), ("e", 14), ("l", 37), ("l", 37), ("o", 31)] {
            act!(|page: &crate::browser::BrowserView| page.press(letter, code, false));
        }
        steps.push(("typed".into(), eval!("document.getElementById('a').value")));
        keyboard!("keyboard_after_typing");

        // Select all, copy, and paste into the other field.
        // The user's clipboard is put back when the test is done.
        let clipboard = cx.update(|_, cx| cx.read_from_clipboard()).map_err(gone)?;
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("selftest-empty".into()))
        })
        .map_err(gone)?;
        let pane_focused = this
            .update_in(cx, |this, window, cx| {
                page(this).is_some_and(|page| page.focus_handle(cx).contains_focused(window, cx))
            })
            .map_err(gone)?;
        steps.push(("gpui_focus_in_pane".into(), pane_focused.into()));
        act!(|page: &crate::browser::BrowserView| page.press("a", 0, true));
        steps.push((
            "selected_after_cmd_a".into(),
            eval!("(e=>e.selectionEnd-e.selectionStart)(document.getElementById('a'))"),
        ));
        act!(|page: &crate::browser::BrowserView| page.press("c", 8, true));
        let copied = cx
            .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .map_err(gone)?;
        steps.push(("clipboard_after_cmd_c".into(), copied.into()));
        let (x, y) = centre(&eval!(&format!("{RECT}('b')")));
        act!(|page: &crate::browser::BrowserView| page.click_at(x, y));
        act!(|page: &crate::browser::BrowserView| page.press("v", 9, true));
        steps.push(("pasted".into(), eval!("document.getElementById('b').value")));
        steps.push(("clicks_seen".into(), eval!("window.clicks")));

        // Per-page zoom scales the web view content independently and resets cleanly.
        let inner_width_100 = eval!("window.innerWidth");
        this.update_in(cx, |this, _, cx| {
            if let Some(page) = page(this) {
                page.update(cx, |page, cx| page.step_zoom(1, cx));
            }
        })
        .map_err(gone)?;
        wait(cx, 250).await;
        let zoom_diag_110 = this
            .update_in(cx, |this, _, cx| {
                page(this).map(|page| page.read(cx).diagnostics())
            })
            .map_err(gone)?
            .unwrap_or_default();
        let inner_width_110 = eval!("window.innerWidth");
        this.update_in(cx, |this, _, cx| {
            if let Some(page) = page(this) {
                page.update(cx, |page, cx| page.step_zoom(0, cx));
            }
        })
        .map_err(gone)?;
        wait(cx, 250).await;
        let zoom_diag_reset = this
            .update_in(cx, |this, _, cx| {
                page(this).map(|page| page.read(cx).diagnostics())
            })
            .map_err(gone)?
            .unwrap_or_default();
        let inner_width_reset = eval!("window.innerWidth");
        steps.push((
            "zoom".into(),
            serde_json::json!({
                "percent": zoom_diag_110["zoom_percent"],
                "percent_reset": zoom_diag_reset["zoom_percent"],
                "inner_width_100": inner_width_100,
                "inner_width_110": inner_width_110,
                "inner_width_reset": inner_width_reset,
            }),
        ));

        // The same from the keyboard while the page has it: the window's
        // shortcuts reach the pane although AppKit's first responder is the
        // web view.
        macro_rules! pane_state {
            () => {{
                this.update_in(cx, |this, _, cx| {
                    page(this).map(|page| {
                        let page = page.read(cx);
                        (page.zoom_percent, page.find_open(), page.has_keyboard())
                    })
                })
                .map_err(gone)?
                .unwrap_or_default()
            }};
        }
        let before_keys = pane_state!();
        act!(|page: &crate::browser::BrowserView| page.press("=", 24, true));
        let after_zoom_in = pane_state!();
        act!(|page: &crate::browser::BrowserView| page.press("0", 29, true));
        let after_zoom_reset = pane_state!();
        act!(|page: &crate::browser::BrowserView| page.press("f", 3, true));
        let after_find = pane_state!();
        steps.push((
            "keys_in_page".into(),
            serde_json::json!({
                "keyboard_in_page": before_keys.2,
                // Key 24 is `=` on a US keyboard and `-` on a Turkish one:
                // which way the zoom went depends on the layout.
                "zoom_changed_by_key": after_zoom_in.0 != before_keys.0,
                "zoom_after_cmd_0": after_zoom_reset.0,
                "find_open_after_cmd_f": after_find.1,
            }),
        ));
        // Find in page opens cleanly and Escape dismisses it back to the page.
        let find_opened = this
            .update_in(cx, |this, window, cx| {
                page(this).is_some_and(|page| {
                    page.update(cx, |page, cx| {
                        page.open_find(window, cx);
                        page.find_open()
                    })
                })
            })
            .map_err(gone)?;
        wait(cx, 150).await;
        let _ = cx.update(|window, cx| {
            window.dispatch_action(Box::new(crate::shell::DismissMenu), cx);
        });
        wait(cx, 200).await;
        let find_after_escape = this
            .update_in(cx, |this, _, cx| {
                page(this).is_some_and(|page| page.read(cx).find_open())
            })
            .map_err(gone)?;
        steps.push((
            "find_escape".into(),
            serde_json::json!({
                "opened": find_opened,
                "open_after_escape": find_after_escape,
            }),
        ));
        // Reload on save: a saved source file loads the page again; what
        // Git ignores (a server's logs, its database) does not.
        let project = this
            .update_in(cx, |this, _, cx| {
                if let Some(page) = page(this) {
                    page.update(cx, |page, cx| page.toggle_reload_on_save(cx));
                }
                this.workspace.project.clone()
            })
            .map_err(gone)?;
        let project = std::path::PathBuf::from(project);
        let _ = std::fs::write(project.join(".gitignore"), "*.log\n");
        // The watcher starts off the main thread, and FSEvents a little later.
        wait(cx, 1200).await;
        let loaded_at = eval!("performance.timeOrigin");
        let _ = std::fs::write(project.join("server.log"), "GET /\n");
        wait(cx, 1500).await;
        let after_ignored = eval!("performance.timeOrigin");
        let _ = std::fs::write(project.join("page.html"), "<p>saved</p>\n");
        wait(cx, 1500).await;
        let after_source = eval!("performance.timeOrigin");
        this.update_in(cx, |this, _, cx| {
            if let Some(page) = page(this) {
                page.update(cx, |page, cx| page.toggle_reload_on_save(cx));
            }
        })
        .map_err(gone)?;
        let _ = std::fs::write(project.join("page.html"), "<p>saved again</p>\n");
        wait(cx, 1500).await;
        let after_off = eval!("performance.timeOrigin");
        steps.push((
            "reload_on_save".into(),
            serde_json::json!({
                "kept_for_ignored_file": loaded_at == after_ignored,
                "reloaded_for_source_file": after_ignored != after_source,
                "kept_when_off": after_source == after_off,
            }),
        ));
        // Something drawn over the panes: the page gives way to a picture
        // of itself, and comes back with the keyboard when it is gone.
        macro_rules! page_state {
            ($name:expr) => {{
                let state = this
                    .update_in(cx, |this, _, cx| {
                        page(this).map(|page| {
                            let page = page.read(cx);
                            let mut state = page.diagnostics();
                            state["keyboard"] = page.has_keyboard().into();
                            state["clip_hidden"] = page.probe(0., 0.)["clip_hidden"].clone();
                            state
                        })
                    })
                    .map_err(gone)?
                    .unwrap_or_default();
                steps.push((
                    $name.into(),
                    serde_json::json!({
                        "covered": state["covered"], "stand_in": state["stand_in"],
                        "placed": state["placed"], "clip_hidden": state["clip_hidden"],
                        "keyboard": state["keyboard"],
                    }),
                ));
            }};
        }
        page_state!("before_overlay");
        this.update_in(cx, |this, window, cx| {
            this.open_quick_open(false, window, cx)
        })
        .map_err(gone)?;
        wait(cx, 900).await;
        page_state!("under_overlay");
        this.update_in(cx, |this, window, cx| this.close_quick_open(window, cx))
            .map_err(gone)?;
        wait(cx, 900).await;
        page_state!("after_overlay");
        // A file the page cannot show is saved, and the pane says so.
        let (x, y) = centre(&eval!(&format!("{RECT}('zip')")));
        act!(|page: &crate::browser::BrowserView| page.click_at(x, y));
        wait(cx, 1500).await;
        let saved = std::env::var_os("MONOCODE_BROWSER_DOWNLOADS")
            .map(|folder| std::path::PathBuf::from(folder).join("file.zip").exists());
        steps.push(("download_saved".into(), saved.into()));
        steps.push((
            "url_after_download".into(),
            this.update_in(cx, |this, _, cx| {
                page(this).map(|page| page.read(cx).url.clone())
            })
            .map_err(gone)?
            .into(),
        ));

        // A link for a new window opens a page of its own, beside this one.
        let (x, y) = centre(&eval!(&format!("{RECT}('blank')")));
        steps.push((
            "blank_target".into(),
            eval!(&format!(
                "(e=>e?e.id||e.tagName:null)(document.elementFromPoint({x},{y}))"
            )),
        ));
        let probe = this
            .update_in(cx, |this, _, cx| {
                page(this).map(|page| page.read(cx).probe(x, y))
            })
            .map_err(gone)?;
        steps.push(("probe_blank".into(), probe.unwrap_or_default()));
        page_state!("state_before_blank");
        let clicks_before = eval!("window.clicks");
        act!(|page: &crate::browser::BrowserView| page.click_at(x, y));
        wait(cx, 1500).await;
        steps.push((
            "blank_clicks".into(),
            serde_json::json!([clicks_before, eval!("window.clicks")]),
        ));
        steps.push((
            "pending_pages".into(),
            this.update_in(cx, |this, _, _| this.pending_browser_pages.len())
                .map_err(gone)?
                .into(),
        ));
        let pages = this
            .update_in(cx, |this, _, cx| {
                let mut pages: Vec<(i64, String)> = (this.workspace.browser_panes.iter())
                    .map(|(id, state)| (*id, state.url.clone()))
                    .collect();
                pages.sort();
                let _ = cx;
                serde_json::json!({
                    "open": pages,
                    "active": this.workspace.active,
                    "active_is_new": this.workspace.active != Some(pane),
                })
            })
            .map_err(gone)?;
        steps.push(("after_new_window".into(), pages));

        // Web Inspector invocation (without emitting an error banner when available).
        let inspect_diag = this
            .update_in(cx, |this, _, cx| {
                page(this).map(|page| {
                    page.update(cx, |page, cx| {
                        page.inspect(cx);
                        page.diagnostics()
                    })
                })
            })
            .map_err(gone)?
            .unwrap_or_default();
        wait(cx, 250).await;
        steps.push(("inspect_diagnostics".into(), inspect_diag));

        if let Some(clipboard) = clipboard {
            cx.update(|_, cx| cx.write_to_clipboard(clipboard))
                .map_err(gone)?;
        }
        Ok(())
    }
}
