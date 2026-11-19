//! The client against a scripted server on in-process pipes.
use super::*;
use crate::code::lsp::fake::{wait_until, FakeServer};

const SHORT: Duration = Duration::from_millis(150);
const LONG: Duration = Duration::from_secs(5);

fn start(fake: &FakeServer) -> (Client, Arc<Mutex<Vec<ClientEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let client = Client::start(
        fake.transport(),
        ClientOptions {
            root: PathBuf::from("/work/app"),
            initialization_options: json!({ "hello": true }),
            settings: json!({ "python": { "pythonPath": "/venv/bin/python" } }),
            patience: None,
        },
        Box::new(move |event| lock(&sink).push(event)),
    )
    .unwrap();
    (client, events)
}

#[test]
fn the_handshake_sends_options_and_reads_capabilities() {
    let fake = FakeServer::new(json!({
        "positionEncoding": "utf-8",
        "hoverProvider": true,
        "completionProvider": { "triggerCharacters": ["."] },
        "definitionProvider": false,
        "textDocumentSync": { "openClose": true, "change": 2 },
        "diagnosticProvider": { "identifier": "x", "interFileDependencies": true },
    }));
    let (client, _) = start(&fake);
    client.initialize(LONG).unwrap();
    let hello = fake.wait_for("initialize");
    assert_eq!(hello["params"]["rootUri"], "file:///work/app");
    assert_eq!(hello["params"]["initializationOptions"]["hello"], true);
    assert_eq!(
        hello["params"]["capabilities"]["textDocument"]["completion"]["completionItem"]
            ["snippetSupport"],
        false
    );
    fake.wait_for("initialized");
    let configured = fake.wait_for("workspace/didChangeConfiguration");
    assert_eq!(
        configured["params"]["settings"]["python"]["pythonPath"],
        "/venv/bin/python"
    );
    assert_eq!(client.encoding(), Encoding::Utf8);
    assert!(client.provides("hoverProvider"));
    assert!(client.provides("completionProvider"));
    assert!(!client.provides("definitionProvider"));
    assert!(!client.provides("referencesProvider"));
    assert!(client.syncs_changes());
    assert_eq!(
        client.pull_diagnostics(),
        Some(PullDiagnostics {
            identifier: Some("x".into()),
            inter_file: true
        })
    );
}

#[test]
fn answers_find_their_requests_in_any_order() {
    let fake = FakeServer::new(json!({}));
    // Hold the first answer back until the second request has arrived.
    let held = Arc::new(Mutex::new(None::<Value>));
    let slot = held.clone();
    fake.on("test/echo", move |message, out| {
        let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": message["params"] });
        if message["params"] == "first" {
            *lock(&slot) = Some(reply);
        } else {
            out.send(&reply);
            if let Some(first) = lock(&slot).take() {
                out.send(&first);
            }
        }
    });
    let (client, _) = start(&fake);
    let client = Arc::new(client);
    let other = client.clone();
    let first = std::thread::spawn(move || {
        other.request("test/echo", json!("first"), LONG, &CancelToken::new())
    });
    wait_until(|| fake.count("test/echo") == 1);
    let second = client.request("test/echo", json!("second"), LONG, &CancelToken::new());
    assert_eq!(second, Ok(json!("second")));
    assert_eq!(first.join().unwrap(), Ok(json!("first")));
}

#[test]
fn a_server_error_is_reported() {
    let fake = FakeServer::new(json!({}));
    fake.on("test/fail", |message, out| {
        out.send(&json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": { "code": -32801, "message": "content modified" },
        }));
    });
    let (client, _) = start(&fake);
    assert_eq!(
        client.request("test/fail", Value::Null, LONG, &CancelToken::new()),
        Err(LspError::Server {
            code: -32801,
            message: "content modified".into()
        })
    );
}

#[test]
fn a_request_that_is_not_answered_times_out_and_is_cancelled() {
    let fake = FakeServer::new(json!({}));
    fake.on("test/never", |_, _| {});
    let (client, _) = start(&fake);
    let outcome = client.request("test/never", Value::Null, SHORT, &CancelToken::new());
    assert_eq!(outcome, Err(LspError::Timeout));
    let asked = fake.wait_for("test/never");
    let cancelled = fake.wait_for("$/cancelRequest");
    assert_eq!(cancelled["params"]["id"], asked["id"]);
    // The client still works afterwards.
    fake.on("test/ok", |message, out| out.result(message, json!(1)));
    assert_eq!(
        client.request("test/ok", Value::Null, LONG, &CancelToken::new()),
        Ok(json!(1))
    );
}

#[test]
fn cancelling_returns_at_once_and_tells_the_server() {
    let fake = FakeServer::new(json!({}));
    fake.on("test/never", |_, _| {});
    let (client, _) = start(&fake);
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let started = Instant::now();
    let waiter = std::thread::spawn(move || {
        client.request("test/never", Value::Null, Duration::from_secs(30), &token)
    });
    let asked = fake.wait_for("test/never");
    cancel.cancel();
    assert_eq!(waiter.join().unwrap(), Err(LspError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        fake.wait_for("$/cancelRequest")["params"]["id"],
        asked["id"]
    );
    assert!(cancel.is_cancelled());
}

#[test]
fn a_cancelled_token_never_sends() {
    let fake = FakeServer::new(json!({}));
    let (client, _) = start(&fake);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        client.request("test/echo", Value::Null, LONG, &cancel),
        Err(LspError::Cancelled)
    );
    assert_eq!(fake.count("test/echo"), 0);
}

#[test]
fn requests_from_the_server_are_answered() {
    let fake = FakeServer::new(json!({}));
    let (client, events) = start(&fake);
    let ask = |id: i64, method: &str, params: Value| {
        fake.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        fake.wait_for_response(id)
    };
    let configuration = ask(
        1,
        "workspace/configuration",
        json!({ "items": [
            { "section": "python" },
            { "section": "python.pythonPath" },
            { "section": "absent.thing" },
            {},
        ] }),
    );
    assert_eq!(
        configuration["result"],
        json!([
            { "pythonPath": "/venv/bin/python" },
            "/venv/bin/python",
            null,
            { "python": { "pythonPath": "/venv/bin/python" } },
        ])
    );
    assert!(client.pull_diagnostics().is_none());
    let registered = ask(
        2,
        "client/registerCapability",
        json!({ "registrations": [
            { "id": "d1", "method": "textDocument/diagnostic",
              "registerOptions": { "identifier": "pull" } },
            { "id": "w1", "method": "workspace/didChangeWatchedFiles" },
        ] }),
    );
    assert_eq!(registered["result"], Value::Null);
    assert!(registered.get("error").is_none());
    assert_eq!(
        client.pull_diagnostics().unwrap().identifier.as_deref(),
        Some("pull")
    );
    assert!(lock(&events).contains(&ClientEvent::PullDiagnosticsRegistered));
    ask(
        3,
        "client/unregisterCapability",
        json!({ "unregisterations": [{ "id": "d1", "method": "textDocument/diagnostic" }] }),
    );
    assert!(client.pull_diagnostics().is_none());
    assert_eq!(
        ask(4, "window/workDoneProgress/create", json!({ "token": "t" }))["result"],
        Value::Null
    );
    assert_eq!(
        ask(5, "workspace/workspaceFolders", Value::Null)["result"],
        json!([{ "uri": "file:///work/app", "name": "app" }])
    );
    ask(6, "workspace/diagnostic/refresh", Value::Null);
    assert!(lock(&events).contains(&ClientEvent::DiagnosticsRefresh));
    let unknown = ask(7, "made/up", Value::Null);
    assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
    assert!(unknown.get("result").is_none());
}

#[test]
fn notifications_become_events() {
    let fake = FakeServer::new(json!({}));
    let (client, events) = start(&fake);
    let tell = |method: &str, params: Value| {
        fake.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    };
    tell(
        "textDocument/publishDiagnostics",
        json!({ "uri": "file:///work/app/a.py", "version": 3, "diagnostics": [{ "message": "m" }] }),
    );
    tell(
        "$/progress",
        json!({ "token": 7, "value": { "kind": "begin", "title": "Indexing" } }),
    );
    tell(
        "$/progress",
        json!({ "token": 7, "value": { "kind": "report" } }),
    );
    tell(
        "$/progress",
        json!({ "token": 7, "value": { "kind": "end" } }),
    );
    tell(
        "window/logMessage",
        json!({ "type": 3, "message": "hello from the server" }),
    );
    tell("made/up", Value::Null);
    wait_until(|| lock(&events).len() == 3 && client.log_tail(5).contains("hello"));
    assert_eq!(
        *lock(&events),
        vec![
            ClientEvent::Diagnostics {
                uri: "file:///work/app/a.py".into(),
                version: Some(3),
                items: vec![json!({ "message": "m" })],
            },
            ClientEvent::ProgressBegin {
                token: "7".into(),
                title: Some("Indexing".into())
            },
            ClientEvent::ProgressEnd { token: "7".into() },
        ]
    );
}

#[test]
fn an_unreadable_body_is_skipped_but_a_broken_frame_ends_the_stream() {
    let fake = FakeServer::new(json!({}));
    fake.on("test/ok", |message, out| out.result(message, json!(1)));
    let (client, events) = start(&fake);
    fake.send_raw(b"Content-Length: 5\r\n\r\n{nope");
    assert_eq!(
        client.request("test/ok", Value::Null, LONG, &CancelToken::new()),
        Ok(json!(1))
    );
    assert!(client.log_tail(10).contains("unreadable message"));
    assert!(!client.is_closed());

    fake.on("test/never", |_, _| {});
    let client = Arc::new(client);
    let other = client.clone();
    let waiter = std::thread::spawn(move || {
        other.request(
            "test/never",
            Value::Null,
            Duration::from_secs(30),
            &CancelToken::new(),
        )
    });
    fake.wait_for("test/never");
    fake.send_raw(b"Content-Type: nonsense\r\n\r\n{}");
    assert_eq!(waiter.join().unwrap(), Err(LspError::Closed));
    wait_until(|| client.is_closed());
    assert!(client.log_tail(10).contains("broken stream"));
    assert!(lock(&events).contains(&ClientEvent::Closed));
    assert_eq!(
        client.request("test/ok", Value::Null, LONG, &CancelToken::new()),
        Err(LspError::Closed)
    );
}

#[test]
fn shutdown_asks_then_says_exit() {
    let fake = FakeServer::new(json!({}));
    let (client, _) = start(&fake);
    client.initialize(LONG).unwrap();
    client.shutdown();
    fake.wait_for("shutdown");
    fake.wait_for("exit");
    wait_until(|| client.is_closed());
    wait_until(|| fake.exited());
}

#[test]
fn dropping_the_client_ends_the_server() {
    let fake = FakeServer::new(json!({}));
    let (client, _) = start(&fake);
    client.initialize(LONG).unwrap();
    drop(client);
    wait_until(|| fake.exited());
}

#[cfg(unix)]
#[test]
fn a_real_process_is_spawned_logged_and_killed_with_its_children() {
    let folder = tempfile::tempdir().unwrap();
    // A launcher that never answers and does its waiting in a child, as
    // a server that runs under an interpreter does.
    let script = "echo \"oops $MARK\" >&2; sleep 60 & echo \"child $!\" >&2; wait";
    let transport = ProcessLauncher
        .launch(&Launch {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            cwd: folder.path().to_path_buf(),
            env: vec![("MARK".into(), "from-env".into())],
        })
        .unwrap();
    let client = Client::start(transport, ClientOptions::default(), Box::new(|_| {})).unwrap();
    let pid = client.pid().unwrap();
    wait_until(|| client.log_tail(5).contains("child "));
    let log = client.log_tail(5);
    assert!(log.contains("oops from-env"), "{log}");
    let child: u32 = log.rsplit("child ").next().unwrap().trim().parse().unwrap();
    assert_eq!(client.initialize(SHORT), Err(LspError::Timeout));
    let alive = |pid: u32| unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    assert!(alive(pid) && alive(child));
    drop(client);
    wait_until(|| !alive(pid) && !alive(child));
}

fn change(uri: &str, text: &str, version: i64) -> Value {
    json!({
        "textDocument": { "uri": uri, "version": version },
        "contentChanges": [{ "text": text }],
    })
}

#[test]
fn a_documents_pending_change_is_replaced_by_its_newest() {
    let outgoing = Outgoing::default();
    let push = |method: &str, uri: &str, version: i64| {
        let message =
            json!({ "jsonrpc": "2.0", "method": method, "params": change(uri, "x", version) });
        outgoing.push(&message, PATIENCE).unwrap();
    };
    push("textDocument/didOpen", "file:///a.py", 1);
    for version in 2..50 {
        push("textDocument/didChange", "file:///a.py", version);
    }
    push("textDocument/didChange", "file:///b.py", 2);
    // Open, one change of a, one of b.
    assert_eq!(outgoing.queued(), 3);
    outgoing.next();
    let newest = outgoing.next().unwrap();
    assert!(String::from_utf8(newest)
        .unwrap()
        .contains("\"version\":49"));
    // A change never takes the place of one that a close or open came after.
    push("textDocument/didChange", "file:///a.py", 50);
    push("textDocument/didClose", "file:///a.py", 0);
    push("textDocument/didOpen", "file:///a.py", 1);
    push("textDocument/didChange", "file:///a.py", 2);
    assert_eq!(outgoing.queued(), 5);
}

#[test]
fn a_ticket_is_unwritten_until_the_message_has_been_written() {
    let outgoing = Outgoing::default();
    let open = json!({
        "method": "textDocument/didOpen", "params": change("file:///a.py", "x", 1),
    });
    let ticket = outgoing.push(&open, PATIENCE).unwrap();
    assert!(outgoing.unwritten(ticket));
    outgoing.next();
    // Taken by the writer is not yet written.
    assert!(outgoing.unwritten(ticket));
    outgoing.written();
    assert!(!outgoing.unwritten(ticket));
    assert!(!outgoing.unwritten(0));
}

#[test]
fn a_queue_that_is_not_read_is_bounded() {
    let outgoing = Outgoing::default();
    let big = "y".repeat(1 << 20);
    let mut refused = None;
    for index in 0..100 {
        let message = json!({
            "jsonrpc": "2.0", "method": "workspace/executeCommand",
            "params": { "text": big, "n": index },
        });
        if let Err(error) = outgoing.push(&message, PATIENCE) {
            refused = Some(error);
            break;
        }
    }
    assert!(matches!(refused, Some(Refused::Stuck(_))), "{refused:?}");
    outgoing.close();
    assert_eq!(outgoing.push(&json!({}), PATIENCE), Err(Refused::Closed));
}

#[cfg(unix)]
#[test]
fn a_server_that_stops_reading_is_killed_and_says_why() {
    let folder = tempfile::tempdir().unwrap();
    let transport = ProcessLauncher
        .launch(&Launch {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 60".into()],
            cwd: folder.path().to_path_buf(),
            env: Vec::new(),
        })
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let client = Client::start(
        transport,
        ClientOptions {
            patience: Some(Duration::from_millis(800)),
            ..ClientOptions::default()
        },
        Box::new(move |event| lock(&sink).push(event)),
    )
    .unwrap();
    let pid = client.pid().unwrap();
    let text = "z".repeat(256 * 1024);
    let uri = "file:///a.py";
    // Typing: far more text than the pipe holds, and the writer is stuck on
    // the first of it. What is queued stays one text.
    for version in 1..20 {
        client.notify("textDocument/didChange", change(uri, &text, version));
    }
    assert!(client.shared.outgoing.queued() <= 2);
    assert_eq!(client.failure(), None);
    std::thread::sleep(Duration::from_millis(900));
    client.notify("textDocument/didChange", change(uri, &text, 200));
    let reason = client.failure().expect("the stalled server is given up");
    assert!(reason.contains("has not read its input"), "{reason}");
    wait_until(|| client.is_closed() && lock(&events).contains(&ClientEvent::Closed));
    let alive = |pid: u32| unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    wait_until(|| !alive(pid));
}

#[test]
fn a_server_that_answers_nothing_and_says_nothing_is_given_up() {
    let fake = FakeServer::new(json!({}));
    fake.on("test/never", |_, _| {});
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let client = Client::start(
        fake.transport(),
        ClientOptions {
            patience: Some(Duration::from_millis(100)),
            ..ClientOptions::default()
        },
        Box::new(move |event| lock(&sink).push(event)),
    )
    .unwrap();
    for _ in 0..3 {
        let asked = client.request(
            "test/never",
            Value::Null,
            Duration::from_millis(60),
            &CancelToken::new(),
        );
        assert_eq!(asked, Err(LspError::Timeout));
    }
    let reason = client.failure().expect("given up");
    assert!(reason.contains("3 requests in a row"), "{reason}");
    wait_until(|| lock(&events).contains(&ClientEvent::Closed));
}

#[test]
fn a_busy_server_that_still_answers_is_left_alone() {
    let fake = FakeServer::new(json!({}));
    let client = Client::start(
        fake.transport(),
        ClientOptions {
            patience: Some(Duration::from_millis(100)),
            ..ClientOptions::default()
        },
        Box::new(|_| {}),
    )
    .unwrap();
    fake.on("test/never", |_, _| {});
    fake.on("test/echo", |message, out| out.result(message, json!(1)));
    for _ in 0..2 {
        let _ = client.request(
            "test/never",
            Value::Null,
            Duration::from_millis(60),
            &CancelToken::new(),
        );
        // An answer in between starts the count again.
        client
            .request("test/echo", Value::Null, LONG, &CancelToken::new())
            .unwrap();
    }
    let _ = client.request(
        "test/never",
        Value::Null,
        Duration::from_millis(150),
        &CancelToken::new(),
    );
    assert_eq!(client.failure(), None);
}

#[test]
fn settings_sections() {
    let settings = json!({ "a": { "b": { "c": 1 } } });
    assert_eq!(settings_section(&settings, Some("a.b.c")), json!(1));
    assert_eq!(settings_section(&settings, Some("a.b")), json!({ "c": 1 }));
    assert_eq!(settings_section(&settings, Some("a.x")), Value::Null);
    assert_eq!(settings_section(&settings, None), settings);
}

#[test]
fn uris_and_paths() {
    let path = Path::new("/work/my app/ş.py");
    let uri = path_to_uri(path);
    assert_eq!(uri, "file:///work/my%20app/%C5%9F.py");
    assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    assert_eq!(uri_to_path("https://example.com/a.py"), None);
}
