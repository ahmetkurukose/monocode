//! The bridge under bad conditions: an app that takes no jobs or no more
//! work, tools that never answer, drop their reply or panic, a caller that
//! gives up, oversized and trickled requests, a hub that has to listen
//! again, and tokens that end.
use super::tests::{FAKE_TOOLS, PROJECT, Pump, SESSION, Stage, quick, said, slow};
use super::*;

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn without_a_pump_a_call_times_out_instead_of_hanging() {
    let stage = Stage::new(Pump::Absent, quick);
    let began = Instant::now();
    let answer = stage.call("browser_echo", json!({}));
    let (text, error) = said(&answer);
    assert!(error);
    assert!(text.starts_with("browser_timeout: "), "{text}");
    assert!(began.elapsed() >= quick(""));
    assert!(began.elapsed() < Duration::from_secs(5));
    // The job was handed over all the same, and is still waiting.
    assert_eq!(stage.queued.lock().unwrap().len(), 1);
    wait_until("the connection to end", || {
        stage.hub.connections.load(Ordering::Relaxed) == 0
    });
}

#[test]
fn an_app_that_takes_no_more_work_is_said_so_at_once() {
    let stage = Stage::new(Pump::Closed, slow);
    let began = Instant::now();
    let (text, error) = said(&stage.call("browser_echo", json!({})));
    assert!(error);
    assert!(text.starts_with("browser_not_started: "), "{text}");
    assert!(began.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_tool_that_never_answers_times_out_and_the_next_call_works() {
    let stage = Stage::new(Pump::Running, quick);
    let (text, error) = said(&stage.call("browser_silent", json!({})));
    assert!(error);
    assert!(text.starts_with("browser_timeout: "), "{text}");
    assert!(!said(&stage.call("browser_echo", json!({}))).1);
}

#[test]
fn a_tool_that_drops_its_reply_or_panics_fails_alone() {
    let stage = Stage::new(Pump::Running, slow);
    for tool in ["browser_forgets", "browser_panics"] {
        let began = Instant::now();
        let (text, error) = said(&stage.call(tool, json!({})));
        assert!(error, "{tool}");
        assert!(text.starts_with("browser_cdp_error: "), "{tool}: {text}");
        // Said at once, not after the call's whole patience.
        assert!(began.elapsed() < Duration::from_secs(5), "{tool}");
    }
    // The main thread goes on taking work.
    assert!(!said(&stage.call("browser_echo", json!({}))).1);
}

#[test]
fn a_project_that_names_nothing_gets_no_capability() {
    let stage = Stage::new(Pump::Absent, quick);
    let exe = Path::new("/bin/monocode");
    for project in ["", "relative/dir", "project", "/"] {
        assert!(
            stage.hub.environment(77, Path::new(project), exe).is_err(),
            "{project:?}"
        );
    }
    assert!(stage.hub.admit(77, "anything").is_err());
}

#[test]
fn a_session_that_ends_loses_its_token() {
    let stage = Stage::new(Pump::Running, slow);
    assert!(!said(&stage.call("browser_echo", json!({}))).1);
    stage.hub.revoke(SESSION);
    assert_eq!(
        said(&stage.call("browser_echo", json!({}))),
        ("Invalid browser capability".into(), true)
    );
    // Started again, it is given a new one.
    let again = stage
        .hub
        .environment(SESSION, Path::new(PROJECT), Path::new("/bin/monocode"))
        .unwrap();
    assert_ne!(again[ENV_TOKEN], stage.environment[ENV_TOKEN]);
    assert!(!said(&forward(&again, "browser_echo", json!({})).unwrap()).1);
}

#[test]
fn the_oldest_sessions_make_room_for_new_ones() {
    let stage = Stage::new(Pump::Absent, quick);
    let exe = Path::new("/bin/monocode");
    for session in 1000..1000 + MAX_SESSIONS as i64 {
        stage
            .hub
            .environment(session, Path::new(PROJECT), exe)
            .unwrap();
    }
    assert_eq!(stage.hub.sessions.lock().unwrap().len(), MAX_SESSIONS);
    // The one the test started with was the oldest.
    assert!(
        stage
            .hub
            .admit(SESSION, &stage.environment[ENV_TOKEN])
            .is_err()
    );
}

#[test]
fn a_hub_whose_thread_ended_is_replaced_with_its_sessions() {
    let stage = Stage::new(Pump::Running, slow);
    let engine = stage.hub.engine.clone();
    let mut slot = Some(stage.hub.clone());
    assert!(Arc::ptr_eq(
        &live_hub(&mut slot, || engine.clone()).unwrap(),
        &stage.hub
    ));
    stage.hub.listening.store(false, Ordering::SeqCst);
    let renewed = live_hub(&mut slot, || engine.clone()).unwrap();
    assert!(!Arc::ptr_eq(&renewed, &stage.hub));
    assert!(renewed.listening.load(Ordering::SeqCst));
    // The token the session was handed still opens its scope.
    assert!(
        renewed
            .admit(SESSION, &stage.environment[ENV_TOKEN])
            .is_ok()
    );
    let mut environment = stage.environment.clone();
    environment.insert(ENV_ENDPOINT.into(), renewed.endpoint.clone());
    assert!(!said(&forward(&environment, "browser_echo", json!({})).unwrap()).1);
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

/// Sends `request` as is to the hub and reads its answer; `None` when the
/// connection ended before a whole answer could be read.
fn raw(stage: &Stage, request: Vec<u8>, close: bool) -> Option<Value> {
    let mut stream = TcpStream::connect(&stage.hub.endpoint).unwrap();
    let mut writer = stream.try_clone().unwrap();
    // The hub may answer before it has read all of it.
    let writing = std::thread::spawn(move || {
        let _ = writer.write_all(&request);
        if close {
            let _ = writer.shutdown(Shutdown::Write);
        }
    });
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut line = String::new();
    let read = BufReader::new(&mut stream).read_line(&mut line);
    writing.join().unwrap();
    read.ok().and_then(|_| serde_json::from_str(&line).ok())
}

#[test]
fn an_oversized_or_unfinished_request_is_refused() {
    let stage = Stage::new(Pump::Running, slow);
    let token = &stage.environment[ENV_TOKEN];
    let huge = json!({"session": SESSION, "token": token, "tool": "browser_echo",
        "arguments": {"text": "x".repeat(MAX_WIRE_BYTES)}});
    // Closing on a peer still writing may reset the connection first.
    if let Some(answer) = raw(&stage, format!("{huge}\n").into_bytes(), false) {
        assert_eq!(said(&answer), ("Invalid browser request".into(), true));
    }
    // A request without its line end, the writer done.
    let cut = json!({"session": SESSION, "token": token, "tool": "browser_echo"});
    let answer = raw(&stage, cut.to_string().into_bytes(), true).unwrap();
    assert_eq!(said(&answer), ("Invalid browser request".into(), true));
    let answer = raw(&stage, b"not json\n".to_vec(), false).unwrap();
    assert_eq!(said(&answer), ("Invalid browser request".into(), true));
    // A request just under the limit is served.
    let fits = json!({"session": SESSION, "token": token, "tool": "browser_echo",
        "arguments": "x".repeat(MAX_WIRE_BYTES - 200)});
    let answer = raw(&stage, format!("{fits}\n").into_bytes(), false).unwrap();
    assert!(!said(&answer).1, "{}", said(&answer).0);
    assert!(FAKE_TOOLS.contains(&"browser_echo"));
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

#[test]
fn a_caller_that_gives_up_frees_the_connection() {
    let stage = Stage::new(Pump::Running, slow);
    let cancel = Arc::new(Cancel::default());
    let hung_up = cancel.clone();
    let giver = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        hung_up.cancel();
    });
    let began = Instant::now();
    let outcome = forward_cancellable(&stage.environment, "browser_silent", json!({}), &cancel);
    giver.join().unwrap();
    assert_eq!(outcome, Err("The call was cancelled.".into()));
    assert!(began.elapsed() < Duration::from_secs(5));
    // The app side noticed and let go well before the call's patience.
    wait_until("the app to stop waiting", || {
        stage.hub.connections.load(Ordering::Relaxed) == 0
    });
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

#[test]
fn calls_are_answered_while_another_waits_and_a_cancelled_one_is_not() {
    let stage = Stage::new(Pump::Running, slow);
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
        "name": "browser_silent", "arguments": {}}}),
    );
    wait_until("the first call to be in flight", || {
        stage.hub.connections.load(Ordering::Relaxed) == 1
    });
    // A call made while the first waits is answered at once.
    line(
        json!({"jsonrpc":"2.0","id":"two","method":"tools/call","params":{
        "name": "browser_echo", "arguments": {}}}),
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
}

#[test]
fn an_answer_too_large_to_send_is_replaced() {
    // Images each under the per-image limit, together over the answer's.
    fn heavy(_: Rc<dyn Host>, call: Call) -> LocalBoxFuture<'static, ()> {
        Box::pin(async move {
            let image = || Content::Image {
                mime: "image/png".into(),
                data: "x".repeat(MAX_IMAGE_BYTES),
            };
            (call.reply)(ToolOutput {
                content: vec![image(), image(), image()],
                is_error: false,
            })
        })
    }
    let stage = Stage::with_run(Pump::Running, slow, heavy, |_| true);
    let (text, error) = said(&stage.call("browser_screenshot", json!({})));
    assert!(error);
    assert!(text.contains("too large"), "{text}");
}
