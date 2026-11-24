//! Code actions beyond a plain list of edits: asking for one kind only,
//! changes that create, rename and delete files, and commands that edit in
//! several rounds or after the action's own edit.
use super::*;
use crate::code::lsp::types::{DocumentChange, TextEdit};

fn acting_rig() -> Rig {
    Rig::with(ManagerOptions::default(), Registry::builtin(), |launch| {
        let mut caps = capabilities(launch);
        if !launch.program.ends_with("ruff") {
            caps["codeActionProvider"] = json!(true);
            caps["renameProvider"] = json!(true);
            caps["executeCommandProvider"] = json!({ "commands": ["go"] });
        }
        Ok(FakeServer::new(caps))
    })
}

fn edit(range: Span, text: &str) -> TextEdit {
    TextEdit {
        range,
        text: text.into(),
    }
}

#[test]
fn actions_of_one_kind_are_asked_for_and_kept_even_when_the_server_sends_more() {
    let rig = acting_rig();
    let a = rig.open("a.py", "x = 1\n");
    let pyright = rig.pyright();
    pyright.on("textDocument/codeAction", |message, out| {
        // This server ignores `only`.
        out.result(
            message,
            json!([
                { "title": "Extract variable", "kind": "refactor.extract" },
                { "title": "Extract function", "kind": "refactor.extract.function" },
                { "title": "Extractor", "kind": "refactor.extractor" },
                { "title": "Inline", "kind": "refactor.inline" },
                { "title": "Run", "command": "go" },
                { "title": "Fix", "kind": "quickfix" },
            ]),
        );
    });
    let found = rig
        .manager
        .code_actions(&a, 0..1, Some("refactor.extract"), &none())
        .unwrap();
    let titles: Vec<_> = found.iter().map(|action| action.title.as_str()).collect();
    assert_eq!(titles, ["Extract variable", "Extract function"]);
    let asked = pyright.wait_for("textDocument/codeAction");
    assert_eq!(
        asked["params"]["context"]["only"],
        json!(["refactor.extract"])
    );

    // Without a kind, everything comes and nothing is asked to be left out.
    let all = rig.manager.code_actions(&a, 0..1, None, &none()).unwrap();
    assert_eq!(all.len(), 6);
    let asked = pyright.received("textDocument/codeAction");
    assert!(asked.last().unwrap()["params"]["context"]
        .get("only")
        .is_none());
}

#[test]
fn file_operations_come_in_order_with_each_edit_counted_on_the_text_before_it() {
    let rig = acting_rig();
    let text = "ş = 1\n";
    let a = rig.open("a.py", text);
    let (b, c, made) = (rig.file("b.py"), rig.file("c.py"), rig.file("new.py"));
    std::fs::write(&b, "gone = 1\n").unwrap();
    let pyright = rig.pyright();
    let (ua, ub, uc, un) = (uri(&a), uri(&b), uri(&c), uri(&made));
    pyright.on("textDocument/rename", move |message, out| {
        out.result(
            message,
            json!({ "documentChanges": [
                { "kind": "create", "uri": un, "options": { "ignoreIfExists": true } },
                { "textDocument": { "uri": un, "version": null }, "edits": [
                    { "range": range(0, 0, 0, 0), "newText": "ğ = 2\n" } ] },
                // Counted on the text the edit before left: `ğ` is one
                // UTF-16 unit and two bytes.
                { "textDocument": { "uri": un, "version": null }, "edits": [
                    { "range": range(0, 1, 0, 1), "newText": "x" } ] },
                { "textDocument": { "uri": ua, "version": 1 }, "edits": [
                    { "range": range(0, 0, 0, 1), "newText": "go" } ] },
                { "kind": "rename", "oldUri": ua, "newUri": uc,
                  "options": { "overwrite": true } },
                // The renamed file holds the edited text.
                { "textDocument": { "uri": uc, "version": null }, "edits": [
                    { "range": range(0, 2, 0, 3), "newText": "" } ] },
                { "kind": "delete", "uri": ub,
                  "options": { "recursive": false, "ignoreIfNotExists": true } },
            ] }),
        );
    });
    let change = rig.manager.rename(&a, 0, "go", &none()).unwrap();
    // The client said it can take them.
    let asked = pyright.wait_for("initialize");
    assert_eq!(
        asked["params"]["capabilities"]["workspace"]["workspaceEdit"]["resourceOperations"],
        json!(["create", "rename", "delete"])
    );
    assert_eq!(change.file_operations(), 3);
    assert_eq!(
        change.changes,
        [
            DocumentChange::Create {
                path: made.clone(),
                overwrite: false,
                ignore_if_exists: true,
            },
            DocumentChange::Edit {
                path: made.clone(),
                edits: vec![edit(span(0, 0, 0, 0), "ğ = 2\n")],
            },
            DocumentChange::Edit {
                path: made.clone(),
                edits: vec![edit(span(0, 2, 0, 2), "x")],
            },
            DocumentChange::Edit {
                path: a.clone(),
                edits: vec![edit(span(0, 0, 0, 2), "go")],
            },
            DocumentChange::Rename {
                from: a.clone(),
                to: c.clone(),
                overwrite: true,
                ignore_if_exists: false,
            },
            DocumentChange::Edit {
                path: c.clone(),
                edits: vec![edit(span(0, 2, 0, 3), "")],
            },
            DocumentChange::Delete {
                path: b.clone(),
                recursive: false,
                ignore_if_not_exists: true,
            },
        ]
    );
    // Nothing is carried out by the manager.
    assert!(b.exists() && !c.exists() && !made.exists());
}

#[test]
fn a_commands_rounds_on_one_file_each_count_on_the_text_the_last_left() {
    let rig = acting_rig();
    let a = rig.open("a.py", "a = 1\n");
    let pyright = rig.pyright();
    let here = uri(&a);
    pyright.on("textDocument/codeAction", |message, out| {
        out.result(
            message,
            json!([{ "title": "Go", "kind": "refactor.rewrite",
                     "command": { "title": "Go", "command": "go" } }]),
        );
    });
    pyright.on("workspace/executeCommand", move |message, out| {
        out.send(
            &json!({ "jsonrpc": "2.0", "id": 910, "method": "workspace/applyEdit",
            "params": { "edit": { "changes": { here.clone(): [
                { "range": range(0, 0, 0, 0), "newText": "ş = 'ğ'\n" } ] } } } }),
        );
        // In UTF-16 units of the text the first round made.
        out.send(
            &json!({ "jsonrpc": "2.0", "id": 911, "method": "workspace/applyEdit",
            "params": { "edit": { "documentChanges": [
                { "textDocument": { "uri": here.clone(), "version": null }, "edits": [
                    { "range": range(0, 5, 0, 6), "newText": "o" } ] } ] } } }),
        );
        out.result(message, Value::Null);
    });
    let actions = rig.manager.code_actions(&a, 0..0, None, &none()).unwrap();
    let change = rig
        .manager
        .run_code_action_command(&a, &actions[0], &none())
        .unwrap();
    assert_eq!(
        pyright.wait_for_response(911)["result"]["applied"],
        json!(true)
    );
    assert_eq!(
        change.changes,
        [
            DocumentChange::Edit {
                path: a.clone(),
                edits: vec![edit(span(0, 0, 0, 0), "ş = 'ğ'\n")],
            },
            DocumentChange::Edit {
                path: a.clone(),
                edits: vec![edit(span(0, 6, 0, 8), "o")],
            },
        ]
    );
}

#[test]
fn an_actions_command_runs_on_the_text_its_edit_left() {
    let rig = acting_rig();
    let a = rig.open("a.py", "a = 1\n");
    let pyright = rig.pyright();
    let here = uri(&a);
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([{ "title": "Add and go", "kind": "refactor.rewrite",
                     "edit": { "changes": { here.clone(): [
                         { "range": range(0, 0, 0, 0), "newText": "ğ = 0\n" } ] } },
                     "command": { "title": "Go", "command": "go" } }]),
        );
    });
    let here = uri(&a);
    pyright.on("workspace/executeCommand", move |message, out| {
        out.send(
            &json!({ "jsonrpc": "2.0", "id": 920, "method": "workspace/applyEdit",
            "params": { "edit": { "changes": { here.clone(): [
                { "range": range(0, 1, 0, 1), "newText": "x" } ] } } } }),
        );
        out.result(message, Value::Null);
    });
    let actions = rig.manager.code_actions(&a, 0..0, None, &none()).unwrap();
    let action = &actions[0];
    assert!(action.command && action.edit.is_some());

    // The editor applies the edit; the manager hears of it a moment later.
    let edited = "ğ = 0\na = 1\n";
    let manager = rig.manager.clone();
    let path = a.clone();
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        manager.change(&path, edited);
    });
    assert!(rig
        .manager
        .await_document_text(&a, edited, Duration::from_secs(5), &none()));
    late.join().unwrap();
    assert!(!rig
        .manager
        .await_document_text(&a, "other", Duration::from_millis(20), &none()));
    assert!(rig
        .manager
        .await_document_text(&rig.file("closed.py"), "", Duration::ZERO, &none()));

    let change = rig
        .manager
        .run_code_action_command(&a, action, &none())
        .unwrap();
    // `ğ` is two bytes: the server's position is in the edited text.
    assert_eq!(
        change.changes,
        [DocumentChange::Edit {
            path: a.clone(),
            edits: vec![edit(span(0, 2, 0, 2), "x")],
        }]
    );
    let changed = pyright.received("textDocument/didChange");
    assert_eq!(
        changed.last().unwrap()["params"]["contentChanges"][0]["text"],
        json!(edited)
    );
}
