//! What the agents' tools rely on in the manager: joining servers without
//! starting any, starting them on request and keeping them for a while,
//! opening a file for one question without a race, versions that never go
//! back, and stopping everything before the app exits.
use super::*;

fn editor_opens_in_a_loop(
    rig: &Rig,
    path: &Path,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let (manager, context) = (rig.manager.clone(), rig.context.clone());
    let (path, stop) = (path.to_path_buf(), stop.clone());
    std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            manager.open(&path, "in_editor = 2\n", &context);
            manager.close(&path);
        }
    })
}

#[test]
fn a_tool_joins_servers_that_run_and_never_starts_one() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.file("a.py");
    std::fs::write(&a, "x = 1\n").unwrap();
    let before = rig.manager.coverage(&a, &rig.context);
    assert_eq!(before.running, Vec::<String>::new());
    assert_eq!(before.absent, ["basedpyright"]);
    let refused = rig.manager.with_running_file(&a, &rig.context, "7", |_| ());
    assert_eq!(refused, Err(LspError::NoServer));
    assert_eq!(rig.launcher.count(), 0);
    assert!(rig.manager.statuses().is_empty());
    assert!(!rig.manager.is_open(&a));

    // The editor has a file of the project open: its server is joined.
    rig.open("other.py", "y = 2\n");
    rig.wait_ready("basedpyright");
    let b = rig.file("b.py");
    std::fs::write(&b, "z = 3\n").unwrap();
    assert_eq!(
        rig.manager.coverage(&b, &rig.context).running,
        ["basedpyright"]
    );
    let joined = rig
        .manager
        .with_running_file(&b, &rig.context, "7", |manager| manager.document_text(&b));
    assert_eq!(joined, Ok(Some(Arc::from("z = 3\n"))));
    assert_eq!(rig.launcher.count(), 1);
    assert!(!rig.manager.is_open(&b));
}

#[test]
fn a_tool_joining_an_editors_copy_sees_the_editors_text() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.file("a.py");
    std::fs::write(&a, "on_disk = 1\n").unwrap();
    rig.manager.open(&a, "in_editor = 2\n", &rig.context);
    let text = rig
        .manager
        .with_running_file(&a, &rig.context, "7", |manager| manager.document_text(&a))
        .unwrap();
    assert_eq!(text.as_deref(), Some("in_editor = 2\n"));
}

#[test]
fn servers_started_for_an_agent_outlive_the_idle_period_while_it_uses_them() {
    let options = ManagerOptions {
        idle_period: Duration::from_secs(300),
        ..ManagerOptions::default()
    };
    let rig = Rig::pyright_only(options);
    let a = rig.file("a.py");
    std::fs::write(&a, "x = 1\n").unwrap();
    let started = rig
        .manager
        .start_servers(&a, &rig.context, "7", Duration::from_secs(5))
        .unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].server, "basedpyright");
    assert_eq!(started[0].state, ServerState::Ready);
    assert!(!rig.manager.is_open(&a), "only the servers are kept");

    // Well past the idle period, and still there.
    rig.advance(Duration::from_secs(600));
    rig.manager.sweep();
    assert_eq!(rig.live(), 1);
    // Using it renews the lease; time that would have run out the first is
    // not enough now.
    rig.manager
        .with_running_file(&a, &rig.context, "7", |_| ())
        .unwrap();
    rig.advance(Duration::from_secs(600));
    rig.manager.sweep();
    assert_eq!(rig.live(), 1);
    // Another agent's use does not extend it.
    rig.manager
        .with_running_file(&a, &rig.context, "8", |_| ())
        .unwrap();
    rig.advance(Duration::from_secs(301));
    rig.manager.sweep();
    assert_eq!(rig.live(), 0);

    // Let go of, the servers follow the idle policy again.
    rig.manager
        .start_servers(&a, &rig.context, "7", Duration::from_secs(5))
        .unwrap();
    rig.manager.release_servers("7");
    rig.advance(Duration::from_secs(301));
    rig.manager.sweep();
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_one_off_open_never_sees_an_empty_document_while_the_editor_comes_and_goes() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.file("a.py");
    std::fs::write(&a, "on_disk = 1\n").unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let editor = editor_opens_in_a_loop(&rig, &a, &stop);
    for _ in 0..300 {
        let text = rig
            .manager
            .with_file(&a, &rig.context, |manager| manager.document_text(&a))
            .unwrap();
        let text = text.expect("open while asked about");
        assert!(
            &*text == "on_disk = 1\n" || &*text == "in_editor = 2\n",
            "{text:?}"
        );
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    editor.join().unwrap();
    assert!(!rig.manager.is_open(&a));
}

#[test]
fn a_reopened_document_never_goes_back_to_a_version_an_earlier_open_used() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.open("a.py", "x = 1\n");
    let pyright = rig.pyright();
    pyright.wait_for("textDocument/didOpen");
    rig.wait_ready("basedpyright");
    rig.manager.change(&a, "x = 12\n");
    assert_eq!(rig.manager.document_diagnostics(&a).unwrap().version, 2);
    rig.manager.close(&a);

    rig.manager.open(&a, "x = 1\n", &rig.context);
    assert_eq!(rig.manager.document_diagnostics(&a).unwrap().version, 3);
    assert_eq!(
        pyright.wait_nth("textDocument/didOpen", 1)["params"]["textDocument"]["version"],
        3
    );
    // The late report of the earlier open is about a text that is gone.
    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "version": 2, "diagnostics": [{
            "range": range(0, 0, 0, 1), "message": "old",
        }] }),
    );
    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "version": 3, "diagnostics": [] }),
    );
    wait_until(|| rig.manager.document_diagnostics(&a).unwrap().servers[0].version == Some(3));
    assert!(rig.manager.diagnostics(&a).is_empty());
    let settled = rig
        .manager
        .diagnostics_settled(&a, Duration::from_secs(5))
        .unwrap();
    assert!(settled.is_empty());
}

#[test]
fn stopping_everything_waits_for_the_goodbyes() {
    let rig = Rig::new();
    rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    rig.wait_ready("ruff");
    let (pyright, ruff) = (rig.pyright(), rig.ruff());
    rig.manager.stop_all();
    // Asked to shut down and answered, before stop_all came back.
    assert_eq!(pyright.count("shutdown"), 1);
    assert_eq!(ruff.count("shutdown"), 1);
    assert_eq!(rig.live(), 0);
}
