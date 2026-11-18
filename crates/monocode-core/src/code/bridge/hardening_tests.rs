//! The bridge under bad conditions: sessions whose folders confine nothing,
//! a caller that gives up, calls that wait while others are made, a peer
//! that trickles bytes, a hub that has to listen again, and tokens that end.
use super::tests::{Stage, SESSION};
use super::*;
use std::sync::mpsc;

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_folder_that_confines_nothing_gets_no_capability() {
    let stage = Stage::new(&[], |_| {});
    let exe = Path::new("/bin/monocode");
    for (cwd, project) in [
        (Path::new(""), stage.root.as_path()),
        (stage.root.as_path(), Path::new("")),
        (Path::new("relative/dir"), stage.root.as_path()),
        (stage.root.as_path(), Path::new("project")),
        (Path::new("/"), Path::new("/")),
    ] {
        assert!(
            stage.hub.environment(77, cwd, project, exe).is_err(),
            "{cwd:?} {project:?}"
        );
    }
    assert!(stage.hub.admit(77, "anything").is_err());
}

#[test]
fn a_checkout_found_above_the_sessions_folder_is_not_trusted() {
    // The app knows nothing of this folder, and a repository around it
    // (a home folder kept in git, say) is none of the session's business.
    let stage = Stage::new(&[], |_| {});
    let home = stage.root.parent().unwrap().join("home");
    let work = home.join("scratch/work");
    std::fs::create_dir_all(home.join(".git")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(home.join("secret.py"), "password = 1\n").unwrap();
    std::fs::write(work.join("mine.py"), "x = 1\n").unwrap();
    let environment = stage
        .hub
        .environment(78, &work, &work, Path::new("/bin/monocode"))
        .unwrap();
    let said = forward(
        &environment,
        "code_symbols",
        json!({"file": home.join("secret.py")}),
    )
    .unwrap_err();
    assert!(said.contains("outside this session's project"), "{said}");
    let outside = forward(
        &environment,
        "run_list",
        json!({"file": home.join("secret.py")}),
    );
    assert!(outside.is_err());
    // Its own folder works, and is where things run.
    let own = forward(
        &environment,
        "run_list",
        json!({"file": work.join("mine.py")}),
    )
    .unwrap();
    assert!(own.contains("Tasks for mine.py"), "{own}");
    // A file next to it in the home folder is not reachable by `..` either.
    let up = forward(&environment, "run_list", json!({"file": "../../secret.py"}));
    assert!(up.is_err());
}

#[test]
fn a_session_that_ends_loses_its_token() {
    let stage = Stage::new(&[], |_| {});
    assert!(stage.call("run_status", json!({})).is_ok());
    stage.hub.revoke(SESSION);
    assert_eq!(
        stage.call("run_status", json!({})).unwrap_err(),
        "Invalid code capability"
    );
    // Started again, it is given a new one.
    let again = stage
        .hub
        .environment(
            SESSION,
            &stage.root,
            &stage.root,
            Path::new("/bin/monocode"),
        )
        .unwrap();
    assert_ne!(again[ENV_TOKEN], stage.environment[ENV_TOKEN]);
    assert!(forward(&again, "run_status", json!({})).is_ok());
}

#[test]
fn the_oldest_sessions_make_room_for_new_ones() {
    let stage = Stage::new(&[], |_| {});
    let exe = Path::new("/bin/monocode");
    for session in 1000..1000 + MAX_SESSIONS as i64 {
        stage
            .hub
            .environment(session, &stage.root, &stage.root, exe)
            .unwrap();
    }
    let sessions = stage.hub.sessions.lock().unwrap().len();
    assert_eq!(sessions, MAX_SESSIONS);
    // The one the test started with was the oldest.
    assert!(stage
        .hub
        .admit(SESSION, &stage.environment[ENV_TOKEN])
        .is_err());
    assert!(stage
        .hub
        .admit(1000 + MAX_SESSIONS as i64 - 1, "x")
        .is_err());
}

#[test]
fn a_hub_whose_thread_ended_is_replaced_with_its_sessions() {
    let stage = Stage::new(&[], |_| {});
    let mut slot = Some(stage.hub.clone());
    assert!(Arc::ptr_eq(
        &live_hub(&mut slot, stage.services).unwrap(),
        &stage.hub
    ));
    stage.hub.listening.store(false, Ordering::SeqCst);
    let renewed = live_hub(&mut slot, stage.services).unwrap();
    assert!(!Arc::ptr_eq(&renewed, &stage.hub));
    assert!(renewed.listening.load(Ordering::SeqCst));
    // The token the session was handed still opens its scope.
    assert!(renewed
        .admit(SESSION, &stage.environment[ENV_TOKEN])
        .is_ok());
    let mut environment = stage.environment.clone();
    environment.insert(ENV_ENDPOINT.into(), renewed.endpoint.clone());
    assert!(forward(&environment, "run_status", json!({})).is_ok());
}

#[test]
fn a_connection_that_ends_badly_frees_its_place() {
    let counter = Arc::new(AtomicUsize::new(1));
    let slot = Slot(counter.clone());
    let ended = std::thread::spawn(move || {
        let _slot = slot;
        panic!("a tool went wrong");
    })
    .join();
    assert!(ended.is_err());
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

#[test]
fn a_peer_that_trickles_bytes_cannot_hold_a_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        for _ in 0..40 {
            if stream.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let (stream, _) = listener.accept().unwrap();
    let began = Instant::now();
    let reader = ByDeadline {
        stream: &stream,
        until: Instant::now() + Duration::from_millis(400),
    };
    let mut line = String::new();
    let error = BufReader::new(reader).read_line(&mut line).unwrap_err();
    // What a read that ran out of time is called depends on the system.
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
    assert!(began.elapsed() < Duration::from_secs(3));
    drop(stream);
    peer.join().unwrap();
}

#[cfg(unix)]
#[test]
fn a_caller_that_gives_up_ends_the_wait_and_leaves_the_program_running() {
    let stage = Stage::new(&[], |_| {});
    let cancel = Arc::new(Cancel::default());
    let hung_up = cancel.clone();
    let giver = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        hung_up.cancel();
    });
    let began = Instant::now();
    let outcome = forward_cancellable(
        &stage.environment,
        "run_start",
        json!({"command": "sleep 30", "wait_s": 30}),
        &cancel,
    );
    giver.join().unwrap();
    assert_eq!(outcome, Err("The call was cancelled.".into()));
    assert!(began.elapsed() < Duration::from_secs(8));
    // The app side noticed and let go of its connection.
    wait_until("the app to stop waiting", || {
        stage.hub.connections.load(Ordering::Relaxed) == 0
    });
    // Nothing the call started was stopped.
    let status = stage.ok("run_status", json!({}));
    assert!(status.contains("running"), "{status}");
    assert!(status.contains("sleep 30"), "{status}");
    stage.ok("run_stop", json!({"run": 1}));
}

/// A reader fed from a channel, ending when the sender is dropped.
struct Feed(mpsc::Receiver<Vec<u8>>, Vec<u8>);

impl Read for Feed {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.1.is_empty() {
            match self.0.recv() {
                Ok(bytes) => self.1 = bytes,
                Err(_) => return Ok(0),
            }
        }
        let count = buffer.len().min(self.1.len());
        buffer[..count].copy_from_slice(&self.1[..count]);
        self.1.drain(..count);
        Ok(count)
    }
}

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink {
    fn frames(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[cfg(unix)]
#[test]
fn calls_are_answered_while_another_waits_and_a_cancelled_one_is_not() {
    let stage = Stage::new(&[], |_| {});
    let (send, feed) = mpsc::channel::<Vec<u8>>();
    let sink = Sink::default();
    let output = sink.clone();
    let environment = stage.environment.clone();
    let server = std::thread::spawn(move || {
        mcp_serve(BufReader::new(Feed(feed, Vec::new())), output, &environment)
    });
    let line = |value: Value| send.send(format!("{value}\n").into_bytes()).unwrap();
    line(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name": "run_start", "arguments": {"command": "sleep 30", "wait_s": 30}}}),
    );
    // A call made while the first waits is answered at once.
    wait_until("the first call to be in flight", || {
        stage.hub.connections.load(Ordering::Relaxed) == 1
    });
    line(
        json!({"jsonrpc":"2.0","id":"two","method":"tools/call","params":{
        "name": "run_status", "arguments": {}}}),
    );
    wait_until("the second answer", || {
        sink.frames().iter().any(|frame| frame["id"] == "two")
    });
    assert!(sink.frames().iter().all(|frame| frame["id"] != 1));
    // The model interrupts the first.
    line(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
        "params":{"requestId": 1}}));
    wait_until("the app to stop waiting", || {
        stage.hub.connections.load(Ordering::Relaxed) == 0
    });
    drop(send);
    assert_eq!(server.join().unwrap(), Ok(()));
    let frames = sink.frames();
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["id"], "two");
    stage.ok("run_stop", json!({"run": 1}));
}

#[cfg(unix)]
#[test]
fn a_command_an_agent_typed_is_not_a_template() {
    let stage = Stage::new(&[("notes.txt", "x\n")], |_| {});
    // `$FILE` is the shell's, not a variable of ours: no "No file is focused".
    let said = stage.ok(
        "run_start",
        json!({"command": "/bin/sh -c 'echo \"[$FILE][$$]\"'", "env": {"FILE": "mine"}, "wait_s": 20}),
    );
    assert!(said.contains("[mine]["), "{said}");
    assert!(!said.contains("No file"), "{said}");
    let bare = stage.ok(
        "run_start",
        json!({"command": "/bin/sh -c 'echo \"[$FILE_DIR$RELATIVE_FILE]\"'", "wait_s": 20}),
    );
    assert!(bare.contains("[]"), "{bare}");
    // Names that are not variable names are said so, not dropped.
    for key in ["BAD-NAME", "A=B", "1ST", "", "with space"] {
        let said = stage.refused("run_start", json!({"command": "true", "env": {key: "x"}}));
        assert!(said.contains("is not a variable name"), "{key}: {said}");
    }
    let said = stage.refused(
        "run_start",
        json!({"command": "true", "env": {"A": "one\ntwo"}}),
    );
    assert!(said.contains("cannot hold a line break"), "{said}");
}

#[cfg(unix)]
#[test]
fn a_crafted_symbol_cannot_move_the_folder_out_of_the_project() {
    let stage = Stage::new(&[("test_a.py", "def test_x(): pass\n")], |_| {});
    // A folder whose name looks like a variable, and a symbol that would
    // climb out of the project if the folder were filled in like a template.
    let tricky = stage.root.join("$SYMBOL");
    std::fs::create_dir_all(&tricky).unwrap();
    let said = stage.ok(
        "run_start",
        json!({
            "command": "/bin/pwd", "cwd": "$SYMBOL", "symbol": "../../..",
            "file": "test_a.py", "wait_s": 20,
        }),
    );
    assert!(said.contains("/project/$SYMBOL"), "{said}");
}
