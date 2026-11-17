//! The manager against scripted servers: which servers a document gets, what
//! they are told, how their answers are merged, and when they are stopped.
//! Time is the test's own, so the idle policy is checked without sleeping.
use super::client::{lock, CancelToken, Launch};
use super::fake::{wait_until, FakeLauncher, FakeServer};
use super::manager::{DocumentContext, Manager, ManagerOptions};
use super::position::{Point, Span};
use super::registry::Registry;
use super::types::{
    CompletionKind, Location, LspError, LspEvent, ServerState, ServerStatus, Severity,
};
use crate::code::toolchain::Toolchain;
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "").unwrap();
}

fn insight() -> Value {
    json!({
        "textDocumentSync": 2,
        "hoverProvider": true,
        "definitionProvider": true,
        "referencesProvider": true,
        "documentSymbolProvider": true,
        "completionProvider": { "triggerCharacters": ["."], "resolveProvider": true },
        "signatureHelpProvider": { "triggerCharacters": ["(", ","] },
    })
}

/// What each fake announces: pyright as an insight server that pushes its
/// diagnostics, ruff as one that is asked for them.
fn capabilities(launch: &Launch) -> Value {
    if launch.program.ends_with("ruff") {
        json!({
            "textDocumentSync": { "openClose": true, "change": 1 },
            "hoverProvider": true,
            "diagnosticProvider": { "identifier": "Ruff", "interFileDependencies": false },
        })
    } else {
        insight()
    }
}

struct Rig {
    manager: Manager,
    launcher: Arc<FakeLauncher>,
    now: Arc<Mutex<Instant>>,
    events: async_channel::Receiver<LspEvent>,
    _folder: tempfile::TempDir,
    root: PathBuf,
    checkout: PathBuf,
    context: DocumentContext,
}

impl Rig {
    fn new() -> Self {
        Self::with(ManagerOptions::default(), Registry::builtin(), |launch| {
            Ok(FakeServer::new(capabilities(launch)))
        })
    }

    /// One kind of server only, which keeps counting servers simple.
    fn pyright_only(options: ManagerOptions) -> Self {
        let mut registry = Registry::builtin();
        registry
            .servers
            .retain(|server| server.id == "basedpyright");
        Self::with(options, registry, |_| Ok(FakeServer::new(insight())))
    }

    fn with(
        options: ManagerOptions,
        registry: Registry,
        factory: impl Fn(&Launch) -> io::Result<FakeServer> + Send + Sync + 'static,
    ) -> Self {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().canonicalize().unwrap();
        let checkout = root.join("repo");
        touch(&root.join("bin/basedpyright-langserver"));
        touch(&root.join("bin/ruff"));
        touch(&checkout.join("pyproject.toml"));
        let launcher = FakeLauncher::new(factory);
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let manager = Manager::with_parts(
            options,
            registry,
            launcher.clone(),
            Arc::new(move || *lock(&clock)),
        );
        let context = DocumentContext {
            checkout: checkout.clone(),
            project: checkout.clone(),
            toolchain: Toolchain {
                venv: None,
                search_path: root.join("bin").to_string_lossy().into_owned(),
                environment: vec![("MARK".into(), "1".into())],
            },
        };
        Self {
            events: manager.subscribe(),
            manager,
            launcher,
            now,
            _folder: folder,
            root,
            checkout,
            context,
        }
    }

    fn advance(&self, by: Duration) {
        *lock(&self.now) += by;
    }

    fn file(&self, name: &str) -> PathBuf {
        self.checkout.join(name)
    }

    fn open(&self, name: &str, text: &str) -> PathBuf {
        let path = self.file(name);
        self.manager.open(&path, text, &self.context);
        path
    }

    fn pyright(&self) -> FakeServer {
        self.launcher.named("basedpyright-langserver")
    }

    fn ruff(&self) -> FakeServer {
        self.launcher.named("ruff")
    }

    fn status(&self, server: &str) -> Option<ServerStatus> {
        let statuses = self.manager.statuses();
        statuses.into_iter().find(|status| status.server == server)
    }

    fn wait_state(&self, server: &str, wanted: impl Fn(&ServerState) -> bool) -> ServerStatus {
        wait_until(|| {
            self.status(server)
                .is_some_and(|status| wanted(&status.state))
        });
        self.status(server).unwrap()
    }

    fn wait_ready(&self, server: &str) -> ServerStatus {
        self.wait_state(server, |state| *state == ServerState::Ready)
    }

    fn live(&self) -> usize {
        let statuses = self.manager.statuses();
        let live = |state: &ServerState| {
            matches!(
                state,
                ServerState::Starting | ServerState::Ready | ServerState::Indexing { .. }
            )
        };
        statuses.iter().filter(|status| live(&status.state)).count()
    }

    fn drain(&self) -> Vec<LspEvent> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }
}

fn uri(path: &Path) -> String {
    super::client::path_to_uri(path)
}

fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
    json!({ "start": { "line": l1, "character": c1 }, "end": { "line": l2, "character": c2 } })
}

fn span(l1: u32, c1: u32, l2: u32, c2: u32) -> Span {
    Span {
        start: Point {
            line: l1,
            column: c1,
        },
        end: Point {
            line: l2,
            column: c2,
        },
    }
}

fn none() -> CancelToken {
    CancelToken::new()
}

#[test]
fn the_first_document_starts_its_servers_and_the_next_shares_them() {
    let rig = Rig::new();
    assert_eq!(rig.launcher.count(), 0);
    touch(&rig.file("pkg/setup.py"));
    let a = rig.open("a.py", "x = 1\n");
    let opened = rig.pyright().wait_for("textDocument/didOpen");
    assert_eq!(
        opened["params"]["textDocument"],
        json!({ "uri": uri(&a), "languageId": "python", "version": 1, "text": "x = 1\n" })
    );
    rig.ruff().wait_for("textDocument/didOpen");
    let pyright = rig.wait_ready("basedpyright");
    assert_eq!(pyright.root, rig.checkout);
    assert_eq!(pyright.name, "basedpyright-langserver");
    assert!(pyright.command.ends_with("basedpyright-langserver --stdio"));
    assert_eq!(pyright.open_documents, 1);
    assert!(rig.wait_ready("ruff").command.ends_with("ruff server"));
    {
        let launched = lock(&rig.launcher.launched);
        assert_eq!(launched[0].0.cwd, rig.checkout);
        assert_eq!(launched[0].0.env, rig.context.toolchain.environment);
    }
    let hello = rig.pyright().wait_for("initialize");
    assert_eq!(hello["params"]["rootUri"], uri(&rig.checkout));
    let settings = rig.pyright().wait_for("workspace/didChangeConfiguration");
    assert_eq!(
        settings["params"]["settings"]["python"]["analysis"]["diagnosticMode"],
        "openFilesOnly"
    );

    // Same root: the same two servers.
    let b = rig.open("lib/b.py", "y = 2\n");
    let second = rig.pyright().wait_nth("textDocument/didOpen", 1);
    assert_eq!(second["params"]["textDocument"]["uri"], uri(&b));
    assert_eq!(rig.launcher.count(), 2);
    assert_eq!(rig.status("basedpyright").unwrap().open_documents, 2);

    // A nearer marker is another root, so other servers.
    rig.open("pkg/c.py", "z = 3\n");
    wait_until(|| rig.launcher.count() == 4);
    let roots: Vec<PathBuf> = rig.manager.statuses().into_iter().map(|s| s.root).collect();
    assert_eq!(
        roots
            .iter()
            .filter(|root| **root == rig.file("pkg"))
            .count(),
        2
    );

    // Files of no known language start nothing and are not documents.
    rig.open("notes.md", "# hi");
    assert!(!rig.manager.is_open(&rig.file("notes.md")));
    assert_eq!(
        rig.manager.hover(&rig.file("notes.md"), 0, &none()),
        Err(LspError::NoServer)
    );
    let started = rig.drain().into_iter().any(|event| {
        matches!(event, LspEvent::ServerChanged(status) if status.state == ServerState::Starting)
    });
    assert!(started);
}

#[test]
fn changes_carry_the_whole_text_and_closing_is_counted() {
    let rig = Rig::new();
    let a = rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    rig.wait_ready("ruff");
    rig.manager.change(&a, "x = 2\n");
    let changed = rig.pyright().wait_for("textDocument/didChange");
    assert_eq!(
        changed["params"],
        json!({
            "textDocument": { "uri": uri(&a), "version": 2 },
            "contentChanges": [{ "text": "x = 2\n" }],
        })
    );
    rig.ruff().wait_for("textDocument/didChange");
    // The same text again is not a change.
    rig.manager.change(&a, "x = 2\n");
    rig.manager.change(&a, "x = 3\n");
    assert_eq!(
        rig.pyright().wait_nth("textDocument/didChange", 1)["params"]["textDocument"]["version"],
        3
    );

    // A second opener keeps the text and has to close too.
    rig.manager.open(&a, "ignored", &rig.context);
    rig.manager.close(&a);
    assert!(rig.manager.is_open(&a));
    assert_eq!(rig.pyright().count("textDocument/didOpen"), 1);
    rig.manager.close(&a);
    assert!(!rig.manager.is_open(&a));
    let closed = rig.pyright().wait_for("textDocument/didClose");
    assert_eq!(closed["params"]["textDocument"]["uri"], uri(&a));
    assert_eq!(rig.status("basedpyright").unwrap().open_documents, 0);
    // Closing what is not open is harmless.
    rig.manager.close(&a);
    rig.manager.change(&a, "nothing");
}

#[test]
fn a_server_that_wants_no_changes_gets_none() {
    let rig = Rig::with(ManagerOptions::default(), Registry::builtin(), |launch| {
        let quiet = launch.program.ends_with("ruff");
        Ok(FakeServer::new(if quiet { json!({}) } else { insight() }))
    });
    let a = rig.open("a.py", "x = 1\n");
    rig.wait_ready("ruff");
    rig.manager.change(&a, "x = 2\n");
    rig.pyright().wait_for("textDocument/didChange");
    rig.manager.close(&a);
    rig.ruff().wait_for("textDocument/didClose");
    assert_eq!(rig.ruff().count("textDocument/didChange"), 0);
}

#[test]
fn pushed_and_pulled_diagnostics_are_merged_and_tagged() {
    let rig = Rig::with(ManagerOptions::default(), Registry::builtin(), |launch| {
        let fake = FakeServer::new(capabilities(launch));
        if launch.program.ends_with("ruff") {
            fake.on("textDocument/diagnostic", |message, out| {
                let items = json!([{
                    "range": range(1, 7, 1, 9), "severity": 2, "code": "F401",
                    "source": "Ruff", "message": "`os` imported but unused",
                }]);
                out.result(message, json!({ "kind": "full", "items": items }));
            });
        }
        Ok(fake)
    });
    let text = "s = '😀' + 1\nimport os\n";
    let a = rig.file("a.py");
    rig.manager.open(&a, text, &rig.context);
    let pull = rig.ruff().wait_for("textDocument/diagnostic");
    assert_eq!(
        pull["params"],
        json!({ "textDocument": { "uri": uri(&a) }, "identifier": "Ruff" })
    );
    rig.pyright().wait_for("textDocument/didOpen");
    rig.pyright().notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "version": 1, "diagnostics": [{
            "range": range(0, 4, 0, 12), "severity": 1, "code": "reportOperatorIssue",
            "source": "basedpyright", "message": "Operator \"+\" not supported",
        }] }),
    );
    wait_until(|| rig.manager.diagnostics(&a).len() == 2);
    let found = rig.manager.diagnostics(&a);
    assert_eq!(found[0].server, "basedpyright");
    assert_eq!(found[0].severity, Severity::Error);
    // UTF-16 character 12 is byte 14: the emoji is two units and four bytes.
    assert_eq!(found[0].range, span(0, 4, 0, 14));
    assert_eq!(found[1].server, "ruff");
    assert_eq!(found[1].code.as_deref(), Some("F401"));
    assert_eq!(found[1].range, span(1, 7, 1, 9));
    let events = rig.drain();
    let changes = events
        .iter()
        .filter(|event| **event == LspEvent::DiagnosticsChanged { path: a.clone() })
        .count();
    assert_eq!(changes, 2);
    assert_eq!(
        rig.manager
            .diagnostics_settled(&a, Duration::from_secs(5))
            .unwrap()
            .len(),
        2
    );

    // A report for a version we have moved past is dropped.
    rig.manager.change(&a, "s = 1\nimport os\n");
    rig.ruff().wait_nth("textDocument/diagnostic", 1);
    rig.pyright().notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "version": 1, "diagnostics": [] }),
    );
    rig.pyright().notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "version": 2, "diagnostics": [] }),
    );
    wait_until(|| rig.manager.diagnostics(&a).len() == 1);
    assert_eq!(rig.manager.diagnostics(&a)[0].server, "ruff");

    // Closing the document takes its diagnostics with it.
    rig.drain();
    rig.manager.close(&a);
    assert!(rig.manager.diagnostics(&a).is_empty());
    assert!(rig
        .drain()
        .contains(&LspEvent::DiagnosticsChanged { path: a }));
}

#[test]
fn pulls_follow_registration_refresh_and_unchanged_reports() {
    let reports = Arc::new(Mutex::new(json!({ "kind": "full", "items": [{
        "range": range(0, 0, 0, 1), "message": "first",
    }] })));
    let source = reports.clone();
    let mut registry = Registry::builtin();
    registry.servers.retain(|server| server.id == "ruff");
    let rig = Rig::with(ManagerOptions::default(), registry, move |_| {
        let fake = FakeServer::new(json!({ "textDocumentSync": 1 }));
        let source = source.clone();
        fake.on("textDocument/diagnostic", move |message, out| {
            out.result(message, lock(&source).clone());
        });
        Ok(fake)
    });
    let a = rig.open("a.py", "x\n");
    let ruff = rig.ruff();
    rig.wait_ready("ruff");
    // Nothing is pulled until the server says it can be asked.
    ruff.wait_for("textDocument/didOpen");
    assert_eq!(ruff.count("textDocument/diagnostic"), 0);
    assert_eq!(
        rig.manager
            .diagnostics_settled(&a, Duration::from_millis(30)),
        Ok(vec![])
    );
    ruff.send(&json!({
        "jsonrpc": "2.0", "id": 90, "method": "client/registerCapability",
        "params": { "registrations": [{
            "id": "pull", "method": "textDocument/diagnostic",
            "registerOptions": { "interFileDependencies": true },
        }] },
    }));
    wait_until(|| rig.manager.diagnostics(&a).len() == 1);
    assert_eq!(rig.manager.diagnostics(&a)[0].message, "first");
    assert!(ruff.wait_for("textDocument/diagnostic")["params"]
        .get("identifier")
        .is_none());

    // "unchanged" keeps what we have, and still counts as an answer.
    *lock(&reports) = json!({ "kind": "unchanged", "resultId": "1" });
    rig.manager.change(&a, "xy\n");
    ruff.wait_nth("textDocument/diagnostic", 1);
    let settled = rig.manager.diagnostics_settled(&a, Duration::from_secs(5));
    assert_eq!(settled.unwrap()[0].message, "first");

    // A refresh asks again for every open document.
    *lock(&reports) = json!({ "kind": "full", "items": [] });
    let b = rig.open("b.py", "y\n");
    wait_until(|| ruff.count("textDocument/diagnostic") == 3);
    ruff.send(&json!({ "jsonrpc": "2.0", "id": 91, "method": "workspace/diagnostic/refresh" }));
    wait_until(|| ruff.count("textDocument/diagnostic") == 5);
    wait_until(|| rig.manager.diagnostics(&a).is_empty());

    // With dependencies between files, a change to one re-asks for all.
    rig.manager.change(&b, "yz\n");
    wait_until(|| ruff.count("textDocument/diagnostic") == 7);
}

#[test]
fn questions_go_to_the_insight_server_in_its_own_positions() {
    let rig = Rig::new();
    let text = "s = '😀'; s.upper()\n";
    let a = rig.open("a.py", text);
    let other = rig.root.join("lib/site-packages/dep.py");
    touch(&other);
    std::fs::write(&other, "def ş(): pass\n").unwrap();
    let pyright = rig.pyright();
    let target = uri(&other);
    pyright.on("textDocument/definition", move |message, out| {
        out.result(
            message,
            json!([{ "targetUri": target, "targetRange": range(0, 0, 0, 13),
                     "targetSelectionRange": range(0, 4, 0, 5) }]),
        );
    });
    let here = uri(&a);
    pyright.on("textDocument/references", move |message, out| {
        out.result(
            message,
            json!([{ "uri": here, "range": range(0, 10, 0, 11) }]),
        );
    });
    pyright.on("textDocument/hover", |message, out| {
        out.result(
            message,
            json!({ "contents": { "kind": "markdown", "value": "```python\ns: str\n```" },
                    "range": range(0, 10, 0, 11) }),
        );
    });
    pyright.on("textDocument/completion", |message, out| {
        out.result(
            message,
            json!({ "isIncomplete": false, "items": [
                { "label": "upper", "kind": 2, "data": 7,
                  "textEdit": { "range": range(0, 12, 0, 17), "newText": "upper" } },
            ] }),
        );
    });
    pyright.on("completionItem/resolve", |message, out| {
        let mut item = message["params"].clone();
        item["documentation"] = json!("Return a copy in upper case.");
        out.result(message, item);
    });
    pyright.on("textDocument/signatureHelp", |message, out| {
        out.result(
            message,
            json!({ "signatures": [{ "label": "upper() -> str", "parameters": [] }] }),
        );
    });
    pyright.on("textDocument/documentSymbol", |message, out| {
        out.result(
            message,
            json!([{ "name": "s", "kind": 13, "range": range(0, 0, 0, 8),
                     "selectionRange": range(0, 0, 0, 1) }]),
        );
    });

    // Byte 12 is the second `s`: UTF-16 character 10.
    let offset = text.find("s.upper").unwrap();
    assert_eq!(offset, 12);
    let definition = rig.manager.definition(&a, offset, &none()).unwrap();
    assert_eq!(
        definition,
        vec![Location {
            path: other.clone(),
            range: span(0, 4, 0, 6)
        }]
    );
    let asked = pyright.wait_for("textDocument/definition");
    assert_eq!(
        asked["params"],
        json!({ "textDocument": { "uri": uri(&a) }, "position": { "line": 0, "character": 10 } })
    );
    let references = rig.manager.references(&a, offset, true, &none()).unwrap();
    assert_eq!(references[0].range, span(0, 12, 0, 13));
    assert_eq!(
        pyright.wait_for("textDocument/references")["params"]["context"],
        json!({ "includeDeclaration": true })
    );
    let hover = rig.manager.hover(&a, offset, &none()).unwrap().unwrap();
    assert_eq!(hover.contents, "```python\ns: str\n```");
    assert_eq!(hover.range, Some(span(0, 12, 0, 13)));

    let list = rig
        .manager
        .completion(&a, offset + 2, Some('.'), &none())
        .unwrap();
    assert_eq!(
        pyright.wait_for("textDocument/completion")["params"]["context"],
        json!({ "triggerKind": 2, "triggerCharacter": "." })
    );
    assert_eq!(list.items[0].kind, CompletionKind::Method);
    assert_eq!(list.items[0].replace, Some(span(0, 14, 0, 19)));
    assert_eq!(list.items[0].documentation, None);
    let resolved = rig
        .manager
        .resolve_completion(&a, &list.items[0], &none())
        .unwrap();
    assert_eq!(
        resolved.documentation.as_deref(),
        Some("Return a copy in upper case.")
    );
    assert_eq!(
        pyright.wait_for("completionItem/resolve")["params"]["data"],
        7
    );
    rig.manager.completion(&a, offset, None, &none()).unwrap();
    assert_eq!(
        pyright.wait_nth("textDocument/completion", 1)["params"]["context"],
        json!({ "triggerKind": 1 })
    );

    let help = rig
        .manager
        .signature_help(&a, offset, &none())
        .unwrap()
        .unwrap();
    assert_eq!(help.signatures[0].label, "upper() -> str");
    let symbols = rig.manager.document_symbols(&a, &none()).unwrap();
    assert_eq!(symbols[0].name, "s");
    let triggers = rig.manager.triggers(&a);
    assert_eq!(triggers.completion, ["."]);
    assert_eq!(triggers.signature_help, ["(", ","]);

    // Ruff offers hover too, and is never asked: it is there to lint.
    let ruff = rig.ruff();
    rig.wait_ready("ruff");
    for method in [
        "textDocument/hover",
        "textDocument/completion",
        "textDocument/definition",
    ] {
        assert_eq!(ruff.count(method), 0);
    }
    assert_eq!(
        rig.manager.hover(&rig.file("unopened.py"), 0, &none()),
        Err(LspError::NotOpen)
    );
}

#[test]
fn a_file_reached_from_a_server_reuses_it() {
    let rig = Rig::new();
    let a = rig.open("a.py", "import dep\n");
    let library = rig.root.join("venv/lib/site-packages/dep.py");
    let elsewhere = rig.root.join("venv/lib/site-packages/other.py");
    touch(&library);
    touch(&elsewhere);
    // A marker out there must not make a root: it is not in the checkout.
    touch(&rig.root.join("venv/lib/site-packages/pyproject.toml"));
    let pyright = rig.pyright();
    let target = uri(&library);
    pyright.on("textDocument/definition", move |message, out| {
        out.result(
            message,
            json!({ "uri": target, "range": range(0, 0, 0, 0) }),
        );
    });
    assert_eq!(
        rig.manager.definition(&a, 7, &none()).unwrap()[0].path,
        library
    );
    rig.wait_ready("ruff");

    rig.manager.open(&library, "", &rig.context);
    let opened = pyright.wait_nth("textDocument/didOpen", 1);
    assert_eq!(opened["params"]["textDocument"]["uri"], uri(&library));
    assert_eq!(rig.launcher.count(), 2);
    assert_eq!(rig.status("basedpyright").unwrap().open_documents, 2);
    // Not reached by navigation, but outside all the same: the checkout's
    // running servers take it rather than new ones rooted out there.
    rig.manager.open(&elsewhere, "", &rig.context);
    pyright.wait_nth("textDocument/didOpen", 2);
    assert_eq!(rig.launcher.count(), 2);
    assert_eq!(rig.manager.statuses().len(), 2);
}

#[test]
fn a_question_waits_for_a_starting_server_and_gives_up_in_time() {
    let gate = Arc::new(Mutex::new(None::<Value>));
    let held = gate.clone();
    let options = ManagerOptions {
        request_timeout: Duration::from_millis(800),
        ..ManagerOptions::default()
    };
    let mut registry = Registry::builtin();
    registry
        .servers
        .retain(|server| server.id == "basedpyright");
    let rig = Rig::with(options, registry, move |_| {
        let fake = FakeServer::new(insight());
        let held = held.clone();
        // The handshake is answered only when the test lets it be.
        fake.on("initialize", move |message, _| {
            *lock(&held) = Some(message.clone());
        });
        fake.on("textDocument/hover", |message, out| {
            out.result(message, json!({ "contents": "late but here" }));
        });
        Ok(fake)
    });
    let a = rig.open("a.py", "x = 1\n");
    rig.wait_state("basedpyright", |state| *state == ServerState::Starting);
    assert_eq!(rig.manager.hover(&a, 0, &none()), Err(LspError::Timeout));

    let manager = rig.manager.clone();
    let path = a.clone();
    let asking = std::thread::spawn(move || manager.hover(&path, 0, &none()));
    let pyright = rig.pyright();
    wait_until(|| lock(&gate).is_some());
    let hello = lock(&gate).take().unwrap();
    pyright.send(&json!({
        "jsonrpc": "2.0", "id": hello["id"], "result": { "capabilities": insight() },
    }));
    let hover = asking.join().unwrap().unwrap().unwrap();
    assert_eq!(hover.contents, "late but here");
    // The document opened while it was starting was sent once it was ready.
    assert_eq!(pyright.count("textDocument/didOpen"), 1);

    // A question nobody wants any more comes back at once.
    pyright.on("textDocument/definition", |_, _| {});
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let manager = rig.manager.clone();
    let asking = std::thread::spawn(move || manager.definition(&a, 0, &token));
    pyright.wait_for("textDocument/definition");
    cancel.cancel();
    assert_eq!(asking.join().unwrap(), Err(LspError::Cancelled));
    pyright.wait_for("$/cancelRequest");
}

#[test]
fn an_unsupported_feature_says_so() {
    let mut registry = Registry::builtin();
    registry
        .servers
        .retain(|server| server.id == "basedpyright");
    let rig = Rig::with(ManagerOptions::default(), registry, |_| {
        Ok(FakeServer::new(json!({ "hoverProvider": true })))
    });
    let a = rig.open("a.py", "x = 1\n");
    assert_eq!(
        rig.manager.definition(&a, 0, &none()),
        Err(LspError::Unsupported)
    );
    assert_eq!(rig.manager.hover(&a, 0, &none()), Ok(None));
}

#[test]
fn an_idle_server_is_stopped_after_the_idle_period() {
    let options = ManagerOptions {
        idle_period: Duration::from_secs(300),
        ..ManagerOptions::default()
    };
    let rig = Rig::pyright_only(options);
    let a = rig.open("a.py", "x = 1\n");
    let first = rig.pyright();
    let id = rig.wait_ready("basedpyright").id;

    // In use: no amount of time stops it.
    rig.advance(Duration::from_secs(3600));
    rig.manager.sweep();
    assert_eq!(rig.live(), 1);

    rig.manager.close(&a);
    rig.advance(Duration::from_secs(299));
    rig.manager.sweep();
    assert_eq!(rig.live(), 1);
    assert_eq!(first.count("shutdown"), 0);

    // Reopened within the period: the clock starts over on the next close.
    rig.manager.open(&a, "x = 1\n", &rig.context);
    rig.advance(Duration::from_secs(10));
    rig.manager.close(&a);
    rig.advance(Duration::from_secs(299));
    rig.manager.sweep();
    assert_eq!(rig.live(), 1);
    assert_eq!(rig.launcher.count(), 1);

    rig.drain();
    rig.advance(Duration::from_secs(1));
    rig.manager.sweep();
    assert!(rig.manager.statuses().is_empty());
    first.wait_for("shutdown");
    first.wait_for("exit");
    wait_until(|| first.exited());
    let stopped = rig.drain().into_iter().any(|event| {
        matches!(event, LspEvent::ServerChanged(status)
            if status.id == id && status.state == ServerState::Stopped)
    });
    assert!(stopped);

    // The next document starts a fresh one.
    rig.manager.open(&a, "x = 1\n", &rig.context);
    wait_until(|| rig.launcher.count() == 2);
    rig.wait_ready("basedpyright");
}

#[test]
fn the_cap_stops_idle_servers_least_recently_used_first() {
    let options = ManagerOptions {
        max_servers: 2,
        ..ManagerOptions::default()
    };
    let rig = Rig::pyright_only(options);
    for name in ["a", "b", "c", "d"] {
        touch(&rig.file(&format!("{name}/pyproject.toml")));
    }
    let root_of = |status: &ServerStatus| {
        status
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let roots = || -> Vec<String> { rig.manager.statuses().iter().map(root_of).collect() };

    let a = rig.open("a/x.py", "");
    rig.advance(Duration::from_secs(1));
    let b = rig.open("b/x.py", "");
    rig.advance(Duration::from_secs(1));
    // Both idle; `b` was used last.
    rig.manager.close(&a);
    rig.manager.close(&b);
    assert_eq!(roots(), ["a", "b"]);

    // A third server is one too many: the one idle longest goes.
    rig.advance(Duration::from_secs(1));
    let c = rig.open("c/x.py", "");
    assert_eq!(roots(), ["b", "c"]);
    rig.launcher.server(0).wait_for("shutdown");

    // Servers with documents are never stopped for the cap.
    rig.manager.open(&b, "", &rig.context);
    rig.open("d/x.py", "");
    assert_eq!(roots(), ["b", "c", "d"]);
    assert_eq!(rig.live(), 3);

    // The moment one of them has no document left, it is.
    rig.manager.close(&c);
    assert_eq!(roots(), ["b", "d"]);
    wait_until(|| rig.launcher.count() == 4);
}

#[test]
fn a_missing_tool_is_reported_with_what_to_install() {
    let rig = Rig::new();
    std::fs::remove_file(rig.root.join("bin/basedpyright-langserver")).unwrap();
    let a = rig.open("a.py", "x = 1\n");
    let status = rig.status("basedpyright").unwrap();
    assert_eq!(
        status.state,
        ServerState::ToolNotFound {
            looked_for: vec![
                "basedpyright-langserver".into(),
                "pyright-langserver".into()
            ],
            hint: "Install it into the project's virtual environment: pip install basedpyright"
                .into(),
        }
    );
    assert_eq!(status.pid, None);
    let reported = rig.drain().into_iter().any(|event| {
        matches!(event, LspEvent::ServerChanged(status)
            if matches!(status.state, ServerState::ToolNotFound { .. }))
    });
    assert!(reported);
    // Ruff still runs, and nothing waits on the server that is not there.
    rig.wait_ready("ruff");
    assert_eq!(rig.manager.hover(&a, 0, &none()), Err(LspError::NoServer));
    assert_eq!(rig.launcher.count(), 1);

    // The fallback is found once it exists, on a restart.
    touch(&rig.root.join("bin/pyright-langserver"));
    rig.manager.restart_server(status.id);
    let restarted = rig.wait_ready("basedpyright");
    assert_eq!(restarted.name, "pyright-langserver");
    let pyright = rig.launcher.named("pyright-langserver");
    pyright.wait_for("textDocument/didOpen");

    // A missing tool nobody needs any more is forgotten.
    std::fs::remove_file(rig.root.join("bin/ruff")).unwrap();
    let b = rig.root.join("repo2/b.py");
    touch(&b);
    let mut context = rig.context.clone();
    context.checkout = rig.root.join("repo2");
    rig.manager.open(&b, "", &context);
    assert_eq!(rig.manager.statuses().len(), 4);
    rig.manager.close(&b);
    let left = rig.manager.statuses();
    assert!(left.iter().all(|status| status.root != context.checkout
        || !matches!(status.state, ServerState::ToolNotFound { .. })));
}

#[test]
fn a_server_that_cannot_start_or_dies_is_failed_with_its_reason() {
    let refuse = Arc::new(Mutex::new(true));
    let refusing = refuse.clone();
    let mut registry = Registry::builtin();
    registry
        .servers
        .retain(|server| server.id == "basedpyright");
    let rig = Rig::with(ManagerOptions::default(), registry, move |_| {
        if *lock(&refusing) {
            return Err(io::Error::other("no such luck"));
        }
        Ok(FakeServer::new(insight()))
    });
    let failed = |state: &ServerState| matches!(state, ServerState::Failed { .. });
    let a = rig.open("a.py", "x = 1\n");
    let status = rig.wait_state("basedpyright", failed);
    let ServerState::Failed { message, .. } = &status.state else {
        unreachable!()
    };
    assert!(
        message.ends_with("could not be started: no such luck"),
        "{message}"
    );
    assert_eq!(rig.manager.hover(&a, 0, &none()), Err(LspError::NoServer));
    assert_eq!(
        rig.manager.diagnostics_settled(&a, Duration::from_secs(5)),
        Ok(vec![])
    );

    // Opening another document tries again.
    *lock(&refuse) = false;
    let b = rig.open("b.py", "y = 2\n");
    rig.wait_ready("basedpyright");
    let pyright = rig.pyright();
    wait_until(|| pyright.count("textDocument/didOpen") == 2);
    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&b), "diagnostics": [{ "range": range(0, 0, 0, 1), "message": "m" }] }),
    );
    wait_until(|| rig.manager.diagnostics(&b).len() == 1);

    // It dies: failed, and what it said goes with it.
    pyright.crash();
    let status = rig.wait_state("basedpyright", failed);
    let ServerState::Failed { message, .. } = &status.state else {
        unreachable!()
    };
    assert_eq!(message, "basedpyright stopped unexpectedly.");
    assert!(rig.manager.diagnostics(&b).is_empty());
    assert_eq!(status.open_documents, 2);

    // A restart brings it back with both documents.
    rig.manager.restart_server(status.id);
    wait_until(|| rig.launcher.count() == 2);
    rig.wait_ready("basedpyright");
    wait_until(|| rig.pyright().count("textDocument/didOpen") == 2);
}

#[test]
fn repeated_failures_stop_the_retrying() {
    let mut registry = Registry::builtin();
    registry
        .servers
        .retain(|server| server.id == "basedpyright");
    let attempts = Arc::new(Mutex::new(0));
    let counter = attempts.clone();
    let rig = Rig::with(ManagerOptions::default(), registry, move |_| {
        *lock(&counter) += 1;
        Err(io::Error::other("still broken"))
    });
    let failed = |state: &ServerState| matches!(state, ServerState::Failed { .. });
    for (index, name) in ["a.py", "b.py", "c.py", "d.py", "e.py"].iter().enumerate() {
        rig.open(name, "");
        wait_until(|| *lock(&attempts) == (index + 1).min(3));
        rig.wait_state("basedpyright", failed);
    }
    assert_eq!(*lock(&attempts), 3);
}

#[test]
fn stopping_and_restarting_by_hand() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.open("a.py", "x = 1\n");
    let first = rig.pyright();
    let id = rig.wait_ready("basedpyright").id;
    first.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&a), "diagnostics": [{ "range": range(0, 0, 0, 1), "message": "m" }] }),
    );
    wait_until(|| rig.manager.diagnostics(&a).len() == 1);

    rig.manager.stop_server(id);
    first.wait_for("shutdown");
    let status = rig.status("basedpyright").unwrap();
    assert_eq!(status.state, ServerState::Stopped);
    assert_eq!(status.pid, None);
    assert!(rig.manager.diagnostics(&a).is_empty());
    assert_eq!(rig.manager.hover(&a, 0, &none()), Err(LspError::NoServer));

    rig.manager.restart_server(id);
    wait_until(|| rig.launcher.count() == 2);
    let second = rig.pyright();
    assert_eq!(
        second.wait_for("textDocument/didOpen")["params"]["textDocument"]["text"],
        "x = 1\n"
    );
    assert_eq!(rig.wait_ready("basedpyright").id, id);

    rig.manager.stop_all();
    second.wait_for("shutdown");
    assert_eq!(rig.live(), 0);
}

#[test]
fn progress_shows_as_indexing() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    let pyright = rig.pyright();
    pyright.notify(
        "$/progress",
        json!({ "token": "t1", "value": { "kind": "begin", "title": "Indexing" } }),
    );
    let busy = rig.wait_state("basedpyright", |state| {
        *state
            == ServerState::Indexing {
                title: Some("Indexing".into()),
            }
    });
    assert_eq!(busy.open_documents, 1);
    pyright.notify(
        "$/progress",
        json!({ "token": "t1", "value": { "kind": "end" } }),
    );
    rig.wait_ready("basedpyright");
}

#[test]
fn a_one_off_question_opens_from_disk_and_closes_again() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.file("a.py");
    std::fs::write(&a, "on_disk = 1\n").unwrap();
    let asked = rig.manager.with_file(&a, &rig.context, |manager| {
        assert!(manager.is_open(&a));
        manager.hover(&a, 0, &none())
    });
    assert_eq!(asked, Ok(Ok(None)));
    assert!(!rig.manager.is_open(&a));
    let pyright = rig.pyright();
    assert_eq!(
        pyright.wait_for("textDocument/didOpen")["params"]["textDocument"]["text"],
        "on_disk = 1\n"
    );
    pyright.wait_for("textDocument/didClose");
    // The server stays for the next question.
    assert_eq!(rig.live(), 1);

    // The editor's copy wins over the disk, and stays open afterwards.
    rig.manager.open(&a, "in_editor = 2\n", &rig.context);
    rig.manager.with_file(&a, &rig.context, |_| ()).unwrap();
    assert!(rig.manager.is_open(&a));
    assert_eq!(
        pyright.wait_nth("textDocument/didOpen", 1)["params"]["textDocument"]["text"],
        "in_editor = 2\n"
    );
    assert_eq!(pyright.count("textDocument/didOpen"), 2);

    let missing = rig
        .manager
        .with_file(&rig.file("gone.py"), &rig.context, |_| ());
    assert!(matches!(missing, Err(LspError::Io(message)) if message.contains("gone.py")));
}

#[test]
fn dropping_the_manager_ends_its_servers() {
    let rig = Rig::new();
    rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    rig.wait_ready("ruff");
    let (pyright, ruff) = (rig.pyright(), rig.ruff());
    drop(rig);
    wait_until(|| pyright.exited() && ruff.exited());
}

#[test]
fn diagnostics_are_told_per_server_with_the_version_they_are_about() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let a = rig.open("a.py", "x = 1\n");
    let pyright = rig.pyright();
    pyright.wait_for("textDocument/didOpen");
    rig.wait_ready("basedpyright");
    let before = rig.manager.document_diagnostics(&a).unwrap();
    assert_eq!(before.version, 1);
    assert_eq!(before.servers[0].version, None, "nothing reported yet");
    let report = |version: i64| {
        pyright.notify(
            "textDocument/publishDiagnostics",
            json!({ "uri": uri(&a), "version": version, "diagnostics": [{
                "range": range(0, 0, 0, 1), "message": "m",
            }] }),
        );
    };
    let reported = |version: i64| {
        wait_until(|| {
            let found = rig.manager.document_diagnostics(&a).unwrap();
            found.servers[0].version == Some(version)
        });
    };
    report(1);
    reported(1);
    rig.drain();
    // The same report on a newer text is still announced: its positions
    // are the new text's.
    rig.manager.change(&a, "x = 12\n");
    let stale = rig.manager.document_diagnostics(&a).unwrap();
    assert_eq!((stale.version, stale.servers[0].version), (2, Some(1)));
    report(2);
    reported(2);
    assert!(rig
        .drain()
        .contains(&LspEvent::DiagnosticsChanged { path: a.clone() }));
    // And said again for the same text, it is not.
    report(2);
    pyright.notify(
        "$/progress",
        json!({ "token": "t", "value": { "kind": "begin" } }),
    );
    rig.wait_state("basedpyright", |state| {
        matches!(state, ServerState::Indexing { .. })
    });
    assert!(!rig
        .drain()
        .contains(&LspEvent::DiagnosticsChanged { path: a.clone() }));
    // A server that is gone has nothing to say about the current text.
    pyright.crash();
    rig.wait_state("basedpyright", |state| {
        matches!(state, ServerState::Failed { .. })
    });
    let after = rig.manager.document_diagnostics(&a).unwrap();
    assert_eq!(after.servers[0].version, Some(2));
    assert!(after.servers[0].items.is_empty());
    assert!(rig
        .manager
        .document_diagnostics(&rig.file("b.py"))
        .is_none());
}

#[test]
fn a_projects_settings_switch_servers_off_and_name_their_executables() {
    use super::manager::ProjectSettings;
    let rig = Rig::new();
    let project = rig.checkout.clone();
    // One server off: the other still serves the document.
    rig.manager.set_project_settings(
        &project,
        ProjectSettings {
            disabled_servers: ["ruff".to_string()].into(),
            ..Default::default()
        },
    );
    let a = rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    assert!(rig.status("ruff").is_none());
    rig.manager.close(&a);

    // An executable set for the project is that or nothing.
    rig.manager.stop_all();
    rig.manager.set_project_settings(
        &project,
        ProjectSettings {
            tool_paths: [("ruff".to_string(), rig.root.join("bin/missing"))].into(),
            ..Default::default()
        },
    );
    rig.open("a.py", "x = 1\n");
    rig.wait_state("ruff", |state| {
        matches!(state, ServerState::ToolNotFound { .. })
    });
    rig.manager.close(&a);

    // The whole project off: nothing is opened, so nothing needs closing.
    rig.manager.stop_all();
    rig.manager.set_project_settings(
        &project,
        ProjectSettings {
            disabled: true,
            ..Default::default()
        },
    );
    let launched = rig.launcher.count();
    rig.open("a.py", "x = 1\n");
    assert!(!rig.manager.is_open(&a));
    assert_eq!(rig.launcher.count(), launched);
    rig.manager.close(&a);

    // Back to the defaults, and the cap can be changed while running.
    rig.manager
        .set_project_settings(&project, ProjectSettings::default());
    assert_eq!(
        rig.manager.project_settings(&project),
        ProjectSettings::default()
    );
    rig.open("a.py", "x = 1\n");
    rig.wait_ready("basedpyright");
    rig.wait_ready("ruff");
    rig.manager.close(&a);
    assert_eq!(rig.live(), 2);
    rig.manager.set_limits(Some((Duration::from_secs(60), 1)));
    assert_eq!(rig.live(), 1, "the idle server over the cap is stopped");
    rig.advance(Duration::from_secs(61));
    rig.manager.sweep();
    assert_eq!(rig.live(), 0);
}

#[test]
fn a_library_in_the_checkouts_virtual_environment_joins_the_checkouts_server() {
    let rig = Rig::pyright_only(ManagerOptions::default());
    let venv = rig.checkout.join(".venv");
    let library = venv.join("lib/site-packages/pkg/mod.py");
    // A marker beside the library would otherwise make its folder a root.
    touch(&venv.join("lib/site-packages/pkg/setup.py"));
    let context = DocumentContext {
        toolchain: Toolchain {
            venv: Some(venv),
            ..rig.context.toolchain.clone()
        },
        ..rig.context.clone()
    };
    let a = rig.file("a.py");
    rig.manager.open(&a, "import pkg.mod\n", &context);
    rig.wait_ready("basedpyright");
    rig.manager.open(&library, "x = 1\n", &context);
    let statuses = rig.manager.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].root, rig.checkout);
    assert_eq!(statuses[0].open_documents, 2);
}
