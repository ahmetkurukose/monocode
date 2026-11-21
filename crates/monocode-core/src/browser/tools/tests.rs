//! The tools over a fake host, fake pages and a scripted CDP peer.

use std::{cell::RefCell, rc::Rc};

use base64::Engine as _;
use serde_json::{Value, json};

use super::fake::{Driver, FakeHost, FakePage, text};
use super::*;
use crate::browser::DialogInfo;

fn node(id: &str, role: &str, name: &str, backend: i64, children: &[&str]) -> Value {
    json!({
        "nodeId": id, "ignored": false, "backendDOMNodeId": backend,
        "role": {"type": "role", "value": role},
        "name": {"type": "computedString", "value": name},
        "childIds": children,
    })
}

/// A login form: a main landmark, a heading, two fields and a button.
fn login() -> Value {
    json!({"nodes": [
        node("1", "RootWebArea", "Log in", 1, &["2"]),
        node("2", "main", "", 2, &["3", "4", "5", "6"]),
        node("3", "heading", "Log in", 3, &[]),
        node("4", "textbox", "Email", 104, &[]),
        node("5", "textbox", "Password", 105, &[]),
        node("6", "button", "Log in", 106, &[]),
    ]})
}

fn code(output: &ToolOutput) -> String {
    text(output).split(':').next().unwrap_or("").to_owned()
}

fn page_with(host: &Rc<FakeHost>, id: i64) -> Rc<FakePage> {
    let page = host.page(id);
    page.script.reply("Accessibility.getFullAXTree", login());
    page
}

/// Page.getFrameTree answers with a loader id the test can change.
fn loader_of(page: &FakePage) -> Rc<RefCell<String>> {
    let loader = Rc::new(RefCell::new("L1".to_owned()));
    let current = loader.clone();
    page.script.on("Page.getFrameTree", move |_, _| {
        Ok(json!({"frameTree": {"frame": {"id": "F", "loaderId": *current.borrow(), "url": "https://example.com/"}}}))
    });
    loader
}

#[test]
fn definitions_and_instructions() {
    let defs = definitions();
    let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
    for name in [
        "browser_tabs",
        "browser_tab_open",
        "browser_tab_close",
        "browser_tab_show",
        "browser_goto",
        "browser_back",
        "browser_forward",
        "browser_reload",
        "browser_wait",
        "browser_snapshot",
        "browser_click",
        "browser_fill",
        "browser_type",
        "browser_press",
        "browser_scroll",
        "browser_screenshot",
        "browser_eval",
        "browser_console",
        "browser_network",
        "browser_dialog",
    ] {
        assert!(names.contains(&name), "{name} is missing");
    }
    for def in &defs {
        let schema = &def["inputSchema"];
        assert_eq!(schema["type"], "object");
        for required in schema["required"].as_array().unwrap() {
            assert!(
                schema["properties"]
                    .get(required.as_str().unwrap())
                    .is_some()
            );
        }
        assert!(def["description"].as_str().unwrap().len() > 20);
    }
    assert!(INSTRUCTIONS.contains("untrusted"));
    assert!(NEEDS_APPROVAL.contains(&"browser_eval"));
    assert!(names.iter().all(|n| n.starts_with("browser_")));
}

#[test]
fn page_targeting() {
    let host = Rc::new(FakeHost::default());
    let mut driver = Driver::new(&host);
    let none = driver.run("browser_snapshot", json!({}));
    assert_eq!(code(&none), "browser_no_page");
    assert!(text(&none).contains("browser_tab_open"));

    host.pages.borrow_mut().push(FakePage::new(1));
    host.pages.borrow_mut().push(FakePage::new(2));
    let which = driver.run("browser_snapshot", json!({}));
    assert_eq!(code(&which), "browser_no_page");
    assert!(text(&which).contains("1 \"Example\" https://example.com/"));

    host.page(2).info.borrow_mut().shown = true;
    let shown = driver.run("browser_snapshot", json!({}));
    assert!(text(&shown).starts_with("Page 2 "), "{}", text(&shown));

    let named = driver.run("browser_snapshot", json!({"page": 1}));
    assert!(text(&named).starts_with("Page 1 "));
    // Page 1 is now the last used, though 2 is the shown one.
    let last = driver.run("browser_snapshot", json!({}));
    assert!(text(&last).starts_with("Page 1 "));
    let tabs = driver.run("browser_tabs", json!({}));
    assert!(text(&tabs).contains("* 1 \"Example\""));
    assert!(text(&tabs).contains("2 \"Example\" https://example.com/ (the user sees it)"));

    let unknown = driver.run("browser_snapshot", json!({"page": 9}));
    assert_eq!(code(&unknown), "browser_no_page");
    let bad = driver.run("browser_snapshot", json!({"page": true}));
    assert!(bad.is_error);
    let tool = driver.run("browser_fly", json!({}));
    assert!(tool.is_error);
}

#[test]
fn tabs_open_show_close() {
    let host = Rc::new(FakeHost::default());
    let mut driver = Driver::new(&host);
    let opened = driver.run(
        "browser_tab_open",
        json!({"url": "localhost:5173", "isolated": true}),
    );
    assert!(!opened.is_error, "{}", text(&opened));
    assert_eq!(
        host.opened.borrow().as_slice(),
        &[("http://localhost:5173/".to_owned(), true)]
    );
    assert!(
        text(&opened)
            .starts_with("Opened page 100 in the background (isolated profile). Page 100 is at")
    );
    // The new page is the current one.
    let snap = driver.run("browser_snapshot", json!({}));
    assert!(text(&snap).starts_with("Page 100 "));

    let refused = driver.run("browser_tab_open", json!({"url": "file:///etc/passwd"}));
    assert_eq!(code(&refused), "browser_url_refused");
    assert_eq!(host.opened.borrow().len(), 1);

    let show = driver.run("browser_tab_show", json!({"page": 100}));
    assert_eq!(text(&show), "Page 100 is shown to the user.");
    let close = driver.run("browser_tab_close", json!({"page": 100}));
    assert_eq!(text(&close), "Closed page 100.");
    let again = driver.run("browser_tab_close", json!({"page": 100}));
    assert_eq!(code(&again), "browser_no_page");
    let gone = driver.run("browser_snapshot", json!({}));
    assert_eq!(code(&gone), "browser_no_page");
}

#[test]
fn a_tab_loaded_for_a_command_is_read_once_its_first_load_ends() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    // A restored tab: no browser yet, its first document on its way.
    *page.cold.borrow_mut() = page.cdp.borrow_mut().take();
    let steps = Rc::new(RefCell::new(Vec::<&str>::new()));
    let looks = Rc::new(std::cell::Cell::new(0));
    let (seen, count) = (steps.clone(), looks.clone());
    page.script.on("Runtime.evaluate", move |p, _| {
        let expression = p["expression"].as_str().unwrap_or("");
        if !expression.starts_with("[location.href") {
            return Ok(json!({"result": {"type": "undefined"}}));
        }
        count.set(count.get() + 1);
        let at = match count.get() {
            1 => json!(["about:blank", "", "complete"]),
            2 => json!(["https://example.com/", "", "loading"]),
            _ => {
                seen.borrow_mut().push("loaded");
                json!(["https://example.com/", "Example", "complete"])
            }
        };
        Ok(json!({"result": {"type": "object", "value": at}}))
    });
    let seen = steps.clone();
    page.script.on("Accessibility.getFullAXTree", move |_, _| {
        seen.borrow_mut().push("snapshot");
        Ok(login())
    });
    let mut driver = Driver::new(&host);
    let snap = driver.run("browser_snapshot", json!({"page": 1}));
    assert!(!snap.is_error, "{}", text(&snap));
    assert_eq!(steps.borrow()[..2], ["loaded", "snapshot"]);
    assert!(looks.get() >= 3);
    // Loaded now: the next command does not wait for it again.
    let before = looks.get();
    driver.run("browser_snapshot", json!({"page": 1}));
    assert!(looks.get() - before <= 1);
}

#[test]
fn goto_waits_for_the_new_document() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    let loader = loader_of(&page);
    let switch = loader.clone();
    page.script.on("Page.navigate", move |p, _| {
        assert_eq!(p["url"], "https://example.org/");
        *switch.borrow_mut() = "L2".into();
        Ok(json!({"frameId": "F", "loaderId": "L2"}))
    });
    let mut driver = Driver::new(&host);
    let out = driver.run("browser_goto", json!({"url": "example.org"}));
    assert_eq!(text(&out), "Page 1 is at https://example.com/ \"Example\".");
    assert_eq!(page.leases.get(), 0, "the lease ends with the command");

    page.script.reply(
        "Page.navigate",
        json!({"frameId": "F", "errorText": "net::ERR_NAME_NOT_RESOLVED"}),
    );
    let failed = driver.run("browser_goto", json!({"url": "https://nope.invalid"}));
    assert!(failed.is_error);
    assert!(text(&failed).contains("net::ERR_NAME_NOT_RESOLVED"));

    let navigations = page.script.calls("Page.navigate").len();
    for url in ["chrome://settings", "devtools://x", "javascript:alert(1)"] {
        let refused = driver.run("browser_goto", json!({"url": url}));
        assert_eq!(code(&refused), "browser_url_refused");
    }
    assert_eq!(page.script.calls("Page.navigate").len(), navigations);
}

#[test]
fn goto_that_does_not_finish_loading_says_so() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script
        .reply("Page.navigate", json!({"frameId": "F", "loaderId": "L9"}));
    let mut driver = Driver::new(&host);
    let out = driver.run("browser_goto", json!({"url": "https://slow.test"}));
    assert!(!out.is_error);
    assert!(text(&out).contains("still loading"));
    assert!(host.clock.now() >= 20_000);
}

#[test]
fn history_steps() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script.reply(
        "Page.getNavigationHistory",
        json!({"currentIndex": 1, "entries": [{"id": 10, "url": "a"}, {"id": 11, "url": "b"}]}),
    );
    let mut driver = Driver::new(&host);
    let back = driver.run("browser_back", json!({}));
    assert!(!back.is_error, "{}", text(&back));
    assert_eq!(
        page.script.calls("Page.navigateToHistoryEntry")[0]["entryId"],
        10
    );
    let forward = driver.run("browser_forward", json!({}));
    assert!(forward.is_error);
    assert!(text(&forward).contains("no later page"));
    let reload = driver.run("browser_reload", json!({}));
    assert!(!reload.is_error);
    assert_eq!(page.script.calls("Page.reload").len(), 1);
}

#[test]
fn snapshot_then_click_by_ref() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    page.script.reply(
        "DOM.getBoxModel",
        json!({"model": {"content": [10, 20, 110, 20, 110, 60, 10, 60]}}),
    );
    let mut driver = Driver::new(&host);
    let snap = driver.run("browser_snapshot", json!({}));
    assert_eq!(
        text(&snap),
        "Page 1 \"Log in\" https://example.com/\n[Main Content]\n  heading \"Log in\"\n  [@e1] textbox \"Email\"\n  [@e2] textbox \"Password\"\n  [@e3] button \"Log in\""
    );
    let set_up = page.script.calls("Target.setAutoAttach");
    assert_eq!(
        set_up[0],
        json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true})
    );

    let click = driver.run("browser_click", json!({"ref": "@e3"}));
    assert_eq!(text(&click), "Clicked @e3 (button \"Log in\").");
    let mouse = page.script.calls("Input.dispatchMouseEvent");
    let kinds: Vec<&str> = mouse.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["mouseMoved", "mousePressed", "mouseReleased"]);
    assert_eq!(
        (mouse[1]["x"].as_f64(), mouse[1]["y"].as_f64()),
        (Some(60.0), Some(40.0))
    );
    assert_eq!(
        (
            mouse[1]["clickCount"].as_i64(),
            mouse[1]["buttons"].as_i64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(
        page.script.calls("DOM.scrollIntoViewIfNeeded")[0]["backendNodeId"],
        106
    );

    let double = driver.run(
        "browser_click",
        json!({"x": 5, "y": 7, "double": true, "button": "right"}),
    );
    assert_eq!(text(&double), "Double-clicked (5, 7).");
    let mouse = page.script.calls("Input.dispatchMouseEvent");
    assert_eq!(mouse.len(), 3 + 5);
    assert_eq!(mouse[7]["clickCount"], 2);
    assert_eq!(mouse[6]["button"], "right");

    let missing = driver.run("browser_click", json!({}));
    assert!(missing.is_error && !text(&missing).starts_with("browser_"));
    assert_eq!(page.leases.get(), 0);
}

#[test]
fn click_reports_a_navigation() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    let loader = loader_of(&page);
    let switch = loader.clone();
    page.script.on("Input.dispatchMouseEvent", move |p, _| {
        if p["type"] == "mouseReleased" {
            *switch.borrow_mut() = "L2".into();
        }
        Ok(json!({}))
    });
    let mut driver = Driver::new(&host);
    driver.run("browser_snapshot", json!({}));
    let click = driver.run("browser_click", json!({"ref": "e3"}));
    assert!(
        text(&click).contains("The page navigated: Page 1 is at"),
        "{}",
        text(&click)
    );
    // Its refs are of the old document now.
    let stale = driver.run("browser_click", json!({"ref": "@e3"}));
    assert_eq!(code(&stale), "browser_stale_ref");
    assert!(text(&stale).contains("navigated"));
    // After a new snapshot the refs work again.
    driver.run("browser_snapshot", json!({}));
    let ok = driver.run("browser_click", json!({"ref": "@e3"}));
    assert!(!ok.is_error, "{}", text(&ok));
}

#[test]
fn refs_unknown_and_recovered() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    let mut driver = Driver::new(&host);
    let early = driver.run("browser_click", json!({"ref": "@e1"}));
    assert_eq!(code(&early), "browser_no_ref");
    assert!(text(&early).contains("browser_snapshot first"));
    driver.run("browser_snapshot", json!({}));
    let unknown = driver.run("browser_click", json!({"ref": "@e99"}));
    assert_eq!(code(&unknown), "browser_no_ref");

    // The page re-rendered: the button is a new node with the same name.
    page.script.on("Runtime.callFunctionOn", |p, _| {
        let connected = p["objectId"] != "obj-106";
        Ok(json!({"result": {"type": "boolean", "value": connected}}))
    });
    page.script.reply(
        "Accessibility.getFullAXTree",
        json!({"nodes": [
            node("1", "RootWebArea", "Log in", 1, &["6"]),
            node("6", "button", "Log in", 206, &[]),
        ]}),
    );
    let click = driver.run("browser_click", json!({"ref": "@e3"}));
    assert!(!click.is_error, "{}", text(&click));
    let scrolled = page.script.calls("DOM.scrollIntoViewIfNeeded");
    assert_eq!(scrolled.last().unwrap()["backendNodeId"], 206);

    // Gone without a match: stale.
    page.script
        .reply("Accessibility.getFullAXTree", json!({"nodes": []}));
    page.script.on("Runtime.callFunctionOn", |_, _| {
        Ok(json!({"result": {"type": "boolean", "value": false}}))
    });
    let lost = driver.run("browser_click", json!({"ref": "@e1"}));
    assert_eq!(code(&lost), "browser_stale_ref");
    assert!(text(&lost).contains("textbox \"Email\""));
}

#[test]
fn user_activity_blocks_input_only() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    let mut driver = Driver::new(&host);
    driver.run("browser_snapshot", json!({}));
    page.user_active.set(true);
    for (tool, args) in [
        ("browser_click", json!({"ref": "@e3"})),
        ("browser_fill", json!({"ref": "@e1", "value": "a"})),
        ("browser_type", json!({"text": "a"})),
        ("browser_press", json!({"key": "Enter"})),
        ("browser_scroll", json!({"dy": 100})),
    ] {
        assert_eq!(
            code(&driver.run(tool, args)),
            "browser_user_active",
            "{tool}"
        );
    }
    assert!(page.script.calls("Input.dispatchMouseEvent").is_empty());
    assert!(!driver.run("browser_snapshot", json!({})).is_error);
}

#[test]
fn an_open_dialog_blocks_page_tools_but_not_its_answer() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    page.script
        .reply("Page.captureScreenshot", json!({"data": tiny_jpeg()}));
    *page.dialog.borrow_mut() = Some(DialogInfo {
        kind: DialogKind::Prompt,
        message: "Your name?".into(),
        default_text: Some("Ann".into()),
    });
    let mut driver = Driver::new(&host);
    let blocked = driver.run("browser_snapshot", json!({}));
    assert_eq!(code(&blocked), "browser_dialog_open");
    assert!(text(&blocked).contains("a prompt dialog \"Your name?\" (default answer \"Ann\")"));
    assert_eq!(
        code(&driver.run("browser_goto", json!({"url": "a.test"}))),
        "browser_dialog_open"
    );
    assert!(!driver.run("browser_screenshot", json!({})).is_error);
    assert!(!driver.run("browser_console", json!({})).is_error);
    assert!(!driver.run("browser_tabs", json!({})).is_error);

    let answer = driver.run("browser_dialog", json!({"action": "accept", "text": "Bo"}));
    assert_eq!(
        text(&answer),
        "Accepted a prompt dialog \"Your name?\" (default answer \"Ann\") on page 1 with \"Bo\"."
    );
    assert_eq!(
        page.answered.borrow().as_slice(),
        &[(true, Some("Bo".to_owned()))]
    );
    let none = driver.run("browser_dialog", json!({"action": "dismiss"}));
    assert!(none.is_error);
    assert!(!driver.run("browser_snapshot", json!({})).is_error);
}

#[test]
fn a_dialog_opened_by_a_click_ends_the_click_and_can_be_answered() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    let mut driver = Driver::new(&host);
    driver.run("browser_click", json!({"x": 1, "y": 1}));
    // The page waits in its click handler: no answer to the press.
    page.script.hold("Input.dispatchMouseEvent");
    let click = driver.call("browser_click", json!({"x": 1, "y": 1}));
    driver.idle();
    assert!(click.borrow().is_none());
    *page.dialog.borrow_mut() = Some(DialogInfo {
        kind: DialogKind::Confirm,
        message: "Delete it?".into(),
        default_text: None,
    });
    // The dialog tool does not wait behind the click.
    let answer = driver.call("browser_dialog", json!({"action": "dismiss"}));
    driver.idle();
    assert!(answer.borrow().is_some());
    *page.dialog.borrow_mut() = Some(DialogInfo {
        kind: DialogKind::Confirm,
        message: "Delete it?".into(),
        default_text: None,
    });
    driver.finish();
    let click = click.borrow_mut().take().unwrap();
    assert!(!click.is_error);
    assert!(text(&click).contains("opened a confirm dialog \"Delete it?\""));
    assert!(host.clock.now() < 1_000);
}

#[test]
fn a_page_runs_one_command_at_a_time_and_pages_run_side_by_side() {
    let host = FakeHost::with_pages(&[1, 2]);
    let one = host.page(1);
    let two = host.page(2);
    one.script.hold("Runtime.evaluate");
    let mut driver = Driver::new(&host);
    let first = driver.call("browser_eval", json!({"page": 1, "expression": "first"}));
    let second = driver.call("browser_eval", json!({"page": 1, "expression": "second"}));
    let other = driver.call("browser_eval", json!({"page": 2, "expression": "other"}));
    driver.idle();
    let sent = |page: &FakePage| -> Vec<String> {
        page.script
            .calls("Runtime.evaluate")
            .iter()
            .map(|p| p["expression"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(sent(&one), ["first"]);
    assert_eq!(sent(&two), ["other"]);
    assert!(other.borrow().is_some(), "page 2 does not wait for page 1");
    assert!(first.borrow().is_none() && second.borrow().is_none());
    one.script.release("Runtime.evaluate");
    driver.idle();
    assert!(first.borrow().is_some());
    assert_eq!(sent(&one), ["first", "second"]);
    assert!(second.borrow().is_some());
}

#[test]
fn commands_time_out_and_free_the_page() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script.hold("Runtime.evaluate");
    let mut driver = Driver::new(&host);
    let slow = driver.call("browser_eval", json!({"expression": "forever"}));
    let next = driver.call("browser_press", json!({"key": "Enter"}));
    driver.idle();
    assert_eq!(page.leases.get(), 1);
    assert!(
        page.script.calls("Input.dispatchKeyEvent").is_empty(),
        "it waits its turn"
    );
    driver.finish();
    let slow = slow.borrow_mut().take().unwrap();
    assert_eq!(code(&slow), "browser_timeout");
    assert!(text(&slow).contains("30 s"));
    assert!(!next.borrow().as_ref().unwrap().is_error);
    assert_eq!(page.leases.get(), 0);
    assert!((30_000..31_000).contains(&host.clock.now()));
}

#[test]
fn waits_end_when_met_or_with_a_timeout() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    let polls = Rc::new(RefCell::new(0));
    let count = polls.clone();
    page.script.on("Runtime.evaluate", move |p, _| {
        let expression = p["expression"].as_str().unwrap();
        if expression.starts_with("[location.href") {
            return Ok(json!({"result": {"value": ["https://example.com/done", "Done", "complete"]}}));
        }
        if expression.contains("querySelector(\"((\")") {
            return Ok(json!({"exceptionDetails": {"exception": {"description": "SyntaxError: '((' is not a valid selector."}}}));
        }
        *count.borrow_mut() += 1;
        let found = expression.contains("\"Welcome\"") && *count.borrow() >= 3;
        Ok(json!({"result": {"type": "boolean", "value": found}}))
    });
    let mut driver = Driver::new(&host);
    let found = driver.run("browser_wait", json!({"text": "Welcome"}));
    assert!(
        text(&found).starts_with("Found text \"Welcome\"."),
        "{}",
        text(&found)
    );
    assert_eq!(host.clock.now(), 200);

    let start = host.clock.now();
    let missing = driver.run(
        "browser_wait",
        json!({"selector": "#nope", "timeout_ms": 1500}),
    );
    assert_eq!(code(&missing), "browser_timeout");
    assert!(text(&missing).contains("1.5 s for selector #nope"));
    assert!(host.clock.now() - start >= 1500);

    let url = driver.run("browser_wait", json!({"url": "*/done"}));
    assert!(!url.is_error);
    let bad = driver.run("browser_wait", json!({"selector": "(("}));
    assert!(bad.is_error && text(&bad).contains("not a valid CSS selector"));
    assert!(driver.run("browser_wait", json!({})).is_error);
    let load = driver.run("browser_wait", json!({"load": "networkidle"}));
    assert!(!load.is_error, "{}", text(&load));

    // At most 120 s, whatever is asked.
    let start = host.clock.now();
    let long = driver.run(
        "browser_wait",
        json!({"text": "never", "timeout_ms": 999_999}),
    );
    assert_eq!(code(&long), "browser_timeout");
    assert!(host.clock.now() - start <= 121_000);
}

#[test]
fn a_closed_page_fails_its_commands_at_once() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script.hold("Runtime.evaluate");
    let mut driver = Driver::new(&host);
    let running = driver.call("browser_eval", json!({"expression": "x"}));
    let queued = driver.call("browser_snapshot", json!({}));
    driver.idle();
    page.script.gone();
    driver.idle();
    assert_eq!(
        code(running.borrow().as_ref().unwrap()),
        "browser_page_gone"
    );
    assert_eq!(code(queued.borrow().as_ref().unwrap()), "browser_page_gone");
    assert_eq!(host.clock.now(), 0);

    let failing = FakeHost::with_pages(&[3]);
    *failing.page(3).load_error.borrow_mut() = Some("Chromium did not start".into());
    let mut driver = Driver::new(&failing);
    assert_eq!(
        code(&driver.run("browser_snapshot", json!({}))),
        "browser_not_started"
    );
}

#[test]
fn big_snapshots_are_cut_with_a_hint() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    let mut nodes = vec![];
    let ids: Vec<String> = (0..3000).map(|i| format!("b{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    nodes.push(node("root", "RootWebArea", "Big", 1, &refs));
    for (i, id) in ids.iter().enumerate() {
        nodes.push(node(
            id,
            "button",
            &format!("Button number {i}"),
            1000 + i as i64,
            &[],
        ));
    }
    page.script
        .reply("Accessibility.getFullAXTree", json!({"nodes": nodes}));
    let mut driver = Driver::new(&host);
    let snap = driver.run("browser_snapshot", json!({}));
    let body = text(&snap);
    assert!(body.len() <= MAX_ANSWER, "{}", body.len());
    assert!(body.contains("[cut at 30 KB: "));
    assert!(
        body.trim_end()
            .ends_with("narrow it with selector (a CSS selector of the part you need) or depth]")
    );

    page.script.reply(
        "Runtime.evaluate",
        json!({"result": {"type": "object", "objectId": "sel-1"}}),
    );
    page.script
        .reply("DOM.describeNode", json!({"node": {"backendNodeId": 1005}}));
    let scoped = driver.run("browser_snapshot", json!({"selector": "#b5"}));
    assert_eq!(
        text(&scoped).lines().nth(1),
        Some("[@e6] button \"Button number 5\"")
    );
}

#[test]
fn iframes_in_their_own_session() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    let mut driver = Driver::new(&host);
    driver.run("browser_snapshot", json!({}));
    page.script.emit(
        "Target.attachedToTarget",
        json!({"sessionId": "S1", "waitingForDebugger": false,
            "targetInfo": {"targetId": "T1", "type": "iframe", "url": "https://pay.test/"}}),
    );
    page.script.reply(
        "Page.getFrameTree",
        json!({"frameTree": {"frame": {"id": "F", "loaderId": "L1", "url": "https://shop.test/"},
            "childFrames": [{"frame": {"id": "T1", "url": "https://pay.test/"}}]}}),
    );
    page.script.on("Accessibility.getFullAXTree", |_, session| {
        Ok(match session {
            Some("S1") => json!({"nodes": [
                node("a", "RootWebArea", "Pay", 1, &["b"]),
                node("b", "textbox", "Card", 7, &[]),
            ]}),
            _ => json!({"nodes": [
                node("1", "RootWebArea", "Shop", 1, &["2"]),
                node("2", "Iframe", "", 50, &[]),
            ]}),
        })
    });
    page.script.on("DOM.getFrameOwner", |p, _| {
        assert_eq!(p["frameId"], "T1");
        Ok(json!({"backendNodeId": 50}))
    });
    page.script.on("DOM.getBoxModel", |p, session| {
        Ok(match (session, p["backendNodeId"].as_i64()) {
            (Some("S1"), Some(7)) => {
                json!({"model": {"content": [10, 10, 30, 10, 30, 20, 10, 20]}})
            }
            (None, Some(50)) => {
                json!({"model": {"content": [100, 200, 400, 200, 400, 300, 100, 300]}})
            }
            other => panic!("unexpected box of {other:?}"),
        })
    });
    let snap = driver.run("browser_snapshot", json!({}));
    assert_eq!(
        text(&snap),
        "Page 1 \"Shop\" https://shop.test/\n[iframe \"https://pay.test/\"]\n  [@e1] textbox \"Card\""
    );
    let click = driver.run("browser_click", json!({"ref": "@e1"}));
    assert!(!click.is_error, "{}", text(&click));
    let mouse = page.script.calls("Input.dispatchMouseEvent");
    assert_eq!(
        (mouse[1]["x"].as_f64(), mouse[1]["y"].as_f64()),
        (Some(120.0), Some(215.0))
    );
    let resolved = page
        .script
        .sent()
        .into_iter()
        .find(|m| m["method"] == "DOM.resolveNode")
        .unwrap();
    assert_eq!(resolved["sessionId"], "S1");

    page.script
        .emit("Target.detachedFromTarget", json!({"sessionId": "S1"}));
    let snap = driver.run("browser_snapshot", json!({}));
    assert!(!text(&snap).contains("Card"));
}

#[test]
fn keys_text_and_fields() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    let mut driver = Driver::new(&host);
    let press = driver.run("browser_press", json!({"key": "Meta+A"}));
    assert_eq!(text(&press), "Pressed Meta+A.");
    let keys = page.script.calls("Input.dispatchKeyEvent");
    assert_eq!(keys[0]["type"], "rawKeyDown");
    assert_eq!(keys[0]["modifiers"], 4);
    assert_eq!(keys[0]["commands"], json!(["selectAll"]));
    assert_eq!(keys[1]["type"], "keyUp");
    assert!(
        driver
            .run("browser_press", json!({"key": "Hyper+Q"}))
            .is_error
    );

    driver.run("browser_snapshot", json!({}));
    let typed = driver.run("browser_type", json!({"text": "Hé\n", "ref": "@e1"}));
    assert_eq!(
        text(&typed),
        "Typed 3 characters into @e1 (textbox \"Email\")."
    );
    assert_eq!(page.script.calls("DOM.focus")[0]["backendNodeId"], 104);
    let keys = page.script.calls("Input.dispatchKeyEvent");
    let downs: Vec<&Value> = keys[2..]
        .iter()
        .filter(|k| k["type"] == "keyDown")
        .collect();
    assert_eq!(downs[0]["text"], "H");
    assert_eq!(downs[0]["modifiers"], 8);
    assert_eq!(downs[1]["key"], "Enter");
    assert_eq!(page.script.calls("Input.insertText")[0]["text"], "é");

    page.script.on("Runtime.callFunctionOn", |p, _| {
        let function = p["functionDeclaration"].as_str().unwrap();
        if function.contains("isConnected") {
            return Ok(json!({"result": {"value": true}}));
        }
        let value = p["arguments"][0]["value"].as_str().unwrap();
        Ok(json!({"result": {"value": match value {
            "red" => json!({"kind": "nooption", "options": ["Blue", "Green"]}),
            "x" => json!({"kind": "other"}),
            _ => json!({"kind": "text"}),
        }}}))
    });
    let fill = driver.run(
        "browser_fill",
        json!({"ref": "@e1", "value": "ann@example.com"}),
    );
    assert_eq!(
        text(&fill),
        "Filled @e1 (textbox \"Email\") with \"ann@example.com\"."
    );
    assert_eq!(
        page.script.calls("Input.insertText")[1]["text"],
        "ann@example.com"
    );
    let cleared = driver.run("browser_fill", json!({"ref": "@e1", "value": ""}));
    assert!(text(&cleared).starts_with("Cleared"));
    let option = driver.run("browser_fill", json!({"ref": "@e1", "value": "red"}));
    assert!(option.is_error && text(&option).contains("\"Blue\", \"Green\""));
    let other = driver.run("browser_fill", json!({"ref": "@e3", "value": "x"}));
    assert!(other.is_error && text(&other).contains("not a text field"));
    let long = driver.run("browser_type", json!({"text": "a".repeat(5001)}));
    assert!(long.is_error && text(&long).contains("browser_fill"));
}

#[test]
fn scrolling() {
    let host = FakeHost::with_pages(&[1]);
    let page = page_with(&host, 1);
    page.script.on("Runtime.evaluate", |p, _| {
        let expression = p["expression"].as_str().unwrap();
        Ok(if expression.starts_with("[Math.round(scrollX)") {
            json!({"result": {"value": [0, 600, 3000, 800]}})
        } else {
            json!({"result": {"value": ["https://example.com/", "Example", "complete"]}})
        })
    });
    let mut driver = Driver::new(&host);
    let out = driver.run("browser_scroll", json!({"dy": 600}));
    assert_eq!(
        text(&out),
        "Scrolled by (0, 600); the page is scrolled to x=0, y=600 of 3000 px (the viewport is 800 px high)."
    );
    let wheel = &page.script.calls("Input.dispatchMouseEvent")[0];
    assert_eq!(wheel["type"], "mouseWheel");
    assert_eq!(
        (wheel["x"].as_f64(), wheel["y"].as_f64()),
        (Some(640.0), Some(400.0))
    );
    driver.run("browser_snapshot", json!({}));
    let into = driver.run("browser_scroll", json!({"ref": "@e3"}));
    assert!(text(&into).starts_with("Scrolled @e3 (button \"Log in\") into view;"));
    assert!(driver.run("browser_scroll", json!({})).is_error);
}

fn tiny_jpeg() -> String {
    let image = image::RgbImage::from_pixel(4, 3, image::Rgb([1, 2, 3]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&image)
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(jpeg)
}

#[test]
fn screenshots() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script
        .reply("Page.captureScreenshot", json!({"data": tiny_jpeg()}));
    page.script.reply(
        "Page.getLayoutMetrics",
        json!({"cssVisualViewport": {"pageX": 0, "pageY": 50, "clientWidth": 3136, "clientHeight": 1000},
            "cssContentSize": {"width": 3136, "height": 90000}}),
    );
    let mut driver = Driver::new(&host);
    let shot = driver.run("browser_screenshot", json!({}));
    assert_eq!(text(&shot), "The visible part of page 1 (4×3 px).");
    assert!(matches!(&shot.content[1], Content::Image { mime, .. } if mime == "image/jpeg"));
    let params = &page.script.calls("Page.captureScreenshot")[0];
    assert_eq!(params["format"], "jpeg");
    assert_eq!(params["clip"]["y"], 50.0);
    assert_eq!(params["clip"]["scale"], 0.5);
    assert_eq!(params["captureBeyondViewport"], false);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shots/full.jpg");
    let full = driver.run(
        "browser_screenshot",
        json!({"full_page": true, "path": path.to_str().unwrap()}),
    );
    assert!(text(&full).contains("its first 16384 CSS px"));
    assert!(text(&full).contains(&format!("Saved to {}.", path.display())));
    assert!(std::fs::read(&path).unwrap().starts_with(&[0xff, 0xd8]));
    let params = &page.script.calls("Page.captureScreenshot")[1];
    assert_eq!(params["captureBeyondViewport"], true);
    assert_eq!(params["clip"]["height"], 16384.0);
}

#[test]
fn eval_results_and_exceptions() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script.on("Runtime.evaluate", |p, _| {
        // By reference: an object is read separately, so the expression
        // runs once whatever it returns.
        assert_ne!(p["returnByValue"], true);
        Ok(match p["expression"].as_str().unwrap() {
            "obj" => json!({"result": {"type": "object", "objectId": "o1"}}),
            "window" => json!({"result": {"type": "object", "className": "Window",
                "description": "Window", "objectId": "w1"}}),
            "body" => json!({"result": {"type": "object", "subtype": "node",
                "className": "HTMLBodyElement", "description": "body#main", "objectId": "n1"}}),
            "nan" => json!({"result": {"type": "number", "unserializableValue": "NaN"}}),
            "nothing" => json!({"result": {"type": "undefined"}}),
            "big" => json!({"result": {"type": "string", "value": "x".repeat(40_000)}}),
            _ => json!({"result": {"type": "object"},
                "exceptionDetails": {"text": "Uncaught", "exception": {"description": "ReferenceError: nope is not defined"}}}),
        })
    });
    page.script.on("Runtime.callFunctionOn", |p, _| {
        assert_eq!(p["returnByValue"], true);
        match p["objectId"].as_str().unwrap() {
            "o1" => Ok(json!({"result": {"type": "object", "value": {"a": [1, 2]}}})),
            "n1" => Ok(json!({"result": {"type": "object", "value": {}}})),
            _ => Err("Object reference chain is too long".into()),
        }
    });
    let mut driver = Driver::new(&host);
    let out = |driver: &mut Driver, e: &str| driver.run("browser_eval", json!({"expression": e}));
    assert_eq!(
        text(&out(&mut driver, "obj")),
        "{\n  \"a\": [\n    1,\n    2\n  ]\n}"
    );
    // Not JSON: described, not an error, and evaluated only once.
    let window = out(&mut driver, "window");
    assert!(!window.is_error);
    assert_eq!(text(&window), "Window");
    let windows = (page.script.calls("Runtime.evaluate").iter())
        .filter(|p| p["expression"] == "window")
        .count();
    assert_eq!(windows, 1);
    assert_eq!(text(&out(&mut driver, "body")), "body#main");
    assert_eq!(page.script.calls("Runtime.releaseObjectGroup").len(), 3);
    assert_eq!(text(&out(&mut driver, "nan")), "NaN");
    assert_eq!(text(&out(&mut driver, "nothing")), "undefined");
    let big = out(&mut driver, "big");
    assert!(text(&big).len() <= MAX_ANSWER && text(&big).contains("return less"));
    let thrown = out(&mut driver, "nope");
    assert!(thrown.is_error);
    assert_eq!(
        text(&thrown),
        "The expression threw: ReferenceError: nope is not defined"
    );
}

#[test]
fn console_and_network_tools() {
    let host = FakeHost::with_pages(&[1]);
    let page = host.page(1);
    page.script.emit(
        "Runtime.consoleAPICalled",
        json!({"type": "error", "timestamp": 0.0, "args": [{"type": "string", "value": "boom"}],
            "stackTrace": {"callFrames": [{"url": "https://example.com/a.js", "lineNumber": 4}]}}),
    );
    page.script.emit(
        "Runtime.consoleAPICalled",
        json!({"type": "log", "timestamp": 0.0, "args": [{"type": "string", "value": "hello"}]}),
    );
    page.script.emit(
        "Network.requestWillBeSent",
        json!({"requestId": "r1", "type": "Fetch", "wallTime": 1.0, "request": {"url": "https://api.test/items", "method": "POST"}}),
    );
    page.script.emit(
        "Network.responseReceived",
        json!({"requestId": "r1", "response": {"status": 201, "mimeType": "application/json"}}),
    );
    page.script.emit(
        "Network.loadingFinished",
        json!({"requestId": "r1", "encodedDataLength": 52.0}),
    );
    page.script.reply(
        "Network.getResponseBody",
        json!({"body": base64::engine::general_purpose::STANDARD.encode("{\"id\":1}"), "base64Encoded": true}),
    );
    let mut driver = Driver::new(&host);
    let errors = text(&driver.run("browser_console", json!({"level": "error"})));
    assert!(errors.starts_with("Console of page 1, oldest first:\n"));
    assert!(errors.contains("error   boom  (https://example.com/a.js:5)"));
    assert!(!errors.contains("hello"));
    assert!(
        driver
            .run("browser_console", json!({"level": "loud"}))
            .is_error
    );
    let all = text(&driver.run("browser_console", json!({"limit": 1})));
    assert!(all.contains("hello") && !all.contains("boom"));

    let network = text(&driver.run("browser_network", json!({"filter": "api.test"})));
    assert!(
        network.ends_with("r1 POST 201 fetch https://api.test/items 52 B"),
        "{network}"
    );
    let body = driver.run("browser_network", json!({"body_of": "r1"}));
    assert_eq!(text(&body), "{\"id\":1}");
    let none = text(&driver.run("browser_network", json!({"filter": "nothing"})));
    assert_eq!(none, "Page 1 has no requests like that.");
}

#[test]
fn cap_cuts_at_a_line_end() {
    let text = (0..5000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let cut = cap(text.clone(), "ask for less");
    assert!(cut.len() <= MAX_ANSWER);
    let kept = cut.lines().count() - 1;
    assert!(cut.ends_with(&format!(
        "[cut at 30 KB: {} more lines left out; ask for less]",
        5000 - kept
    )));
    assert_eq!(cap("short".into(), "x"), "short");
}
