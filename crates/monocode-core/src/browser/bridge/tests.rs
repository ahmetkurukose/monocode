//! The bridge with a main thread of its own: a pump that runs jobs with a
//! fake host and fake tools, the capability, the shape of answers, the stdio
//! framing and the provider wiring.
use super::*;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Wake},
};

pub(super) const SESSION: i64 = 41;
pub(super) const PROJECT: &str = "/work/project";

/// A host with no pages: the fake tools only need its timer.
struct FakeHost;

impl Host for FakeHost {
    fn engine_started(&self) -> bool {
        true
    }

    fn pages(&self, _: &Scope) -> Vec<Rc<dyn super::super::Page>> {
        Vec::new()
    }

    fn open(
        &self,
        _: &Scope,
        _: &str,
        _: bool,
    ) -> LocalBoxFuture<'static, Result<Rc<dyn super::super::Page>, String>> {
        Box::pin(async { Err("no pages here".to_owned()) })
    }

    fn close(&self, _: &Scope, _: super::super::PageId) -> bool {
        false
    }

    fn show(&self, _: &Scope, _: super::super::PageId) -> bool {
        false
    }

    fn sleep(&self, duration: Duration) -> LocalBoxFuture<'static, ()> {
        std::thread::sleep(duration);
        Box::pin(async {})
    }
}

/// A screenshot's worth of base64.
pub(super) fn picture() -> String {
    "QUJD".repeat(300_000)
}

/// The fake tools: each tells the bridge something different.
pub(super) fn fake_run(host: Rc<dyn Host>, call: Call) -> LocalBoxFuture<'static, ()> {
    Box::pin(async move {
        match call.tool.as_str() {
            "browser_echo" => {
                host.sleep(Duration::from_millis(1)).await;
                (call.reply)(ToolOutput::text(format!(
                    "{} {} {} {}",
                    call.scope.session,
                    call.scope.project,
                    host.engine_started(),
                    call.arguments
                )))
            }
            "browser_screenshot" => (call.reply)(ToolOutput {
                content: vec![
                    Content::Text("A picture:".into()),
                    Content::Image {
                        mime: "image/jpeg".into(),
                        data: picture(),
                    },
                ],
                is_error: false,
            }),
            "browser_stale" => (call.reply)(ToolOutput::error("browser_stale_ref", "@e3 is gone")),
            // Holds its reply for ever without answering.
            "browser_silent" => std::mem::forget(call.reply),
            "browser_forgets" => drop(call.reply),
            "browser_panics" => panic!("a tool went wrong"),
            other => (call.reply)(ToolOutput::text(format!("ran {other}"))),
        }
    })
}

pub(super) const FAKE_TOOLS: &[&str] = &[
    "browser_echo",
    "browser_screenshot",
    "browser_stale",
    "browser_silent",
    "browser_forgets",
    "browser_panics",
    "browser_eval",
];

/// Polls a future on this thread until it is ready.
pub(super) fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

/// What runs the jobs a hub submits.
pub(super) enum Pump {
    /// A thread standing in for the main thread, with a fake host.
    Running,
    /// Nobody: jobs are queued and never run, as before the app pumps.
    Absent,
    /// The app takes no more work.
    Closed,
}

/// A hub of its own and one session.
pub(super) struct Stage {
    pub hub: Arc<Hub>,
    pub environment: HashMap<String, String>,
    /// Jobs nobody runs, kept so their replies stay unanswered.
    pub queued: Arc<Mutex<Vec<Job>>>,
}

pub(super) fn quick(_: &str) -> Duration {
    Duration::from_millis(600)
}

pub(super) fn slow(_: &str) -> Duration {
    Duration::from_secs(20)
}

impl Stage {
    pub fn new(pump: Pump, patience: fn(&str) -> Duration) -> Self {
        Self::with_run(pump, patience, fake_run, |name| FAKE_TOOLS.contains(&name))
    }

    pub fn with_run(
        pump: Pump,
        patience: fn(&str) -> Duration,
        run: fn(Rc<dyn Host>, Call) -> LocalBoxFuture<'static, ()>,
        known: fn(&str) -> bool,
    ) -> Self {
        let queued: Arc<Mutex<Vec<Job>>> = Arc::default();
        let submit: Arc<dyn Fn(Job) -> bool + Send + Sync> = match pump {
            Pump::Running => {
                let (send, receive) = mpsc::channel::<Job>();
                std::thread::spawn(move || {
                    let host: Rc<dyn Host> = Rc::new(FakeHost);
                    while let Ok(job) = receive.recv() {
                        block_on(job(host.clone()));
                    }
                });
                let send = Mutex::new(send);
                Arc::new(move |job| send.lock().unwrap().send(job).is_ok())
            }
            Pump::Absent => {
                let queued = queued.clone();
                Arc::new(move |job| {
                    queued.lock().unwrap().push(job);
                    true
                })
            }
            Pump::Closed => Arc::new(|_| false),
        };
        let engine = Engine {
            known: Arc::new(known),
            submit,
            run,
            patience,
        };
        let hub = Hub::start(engine).unwrap();
        let environment = hub
            .environment(SESSION, Path::new(PROJECT), Path::new("/bin/monocode"))
            .unwrap();
        Stage {
            hub,
            environment,
            queued,
        }
    }

    /// One call over the wire, as the stdio server makes it.
    pub fn call(&self, tool: &str, arguments: Value) -> Value {
        forward(&self.environment, tool, arguments).unwrap()
    }
}

/// The text of an answer's first item, and whether it is an error.
pub(super) fn said(answer: &Value) -> (String, bool) {
    (
        answer["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        answer["isError"] == true,
    )
}

#[test]
fn a_call_reaches_the_main_thread_with_its_scope_and_comes_back() {
    let stage = Stage::new(Pump::Running, slow);
    assert!(configured(&stage.environment));
    let answer = stage.call("browser_echo", json!({"url": "http://localhost:3000"}));
    assert_eq!(
        said(&answer),
        (
            format!("{SESSION} {PROJECT} true {{\"url\":\"http://localhost:3000\"}}"),
            false
        )
    );
    // A failure keeps its stable code and is marked as one.
    let stale = stage.call("browser_stale", json!({}));
    assert_eq!(
        said(&stale),
        ("browser_stale_ref: @e3 is gone".into(), true)
    );
    // Calls in a row each get their own answer.
    for round in 0..5 {
        let answer = stage.call("browser_echo", json!(round));
        assert!(said(&answer).0.ends_with(&format!(" {round}")), "{answer}");
    }
}

#[test]
fn an_image_comes_back_as_mcp_image_content() {
    let stage = Stage::new(Pump::Running, slow);
    let answer = stage.call("browser_screenshot", json!({}));
    assert_eq!(answer["isError"], false);
    let content = answer["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0], json!({"type":"text","text":"A picture:"}));
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/jpeg");
    // More than a request may carry, and all of it arrives.
    let data = content[1]["data"].as_str().unwrap();
    assert!(data.len() > MAX_WIRE_BYTES);
    assert_eq!(data, picture());
    // And through the stdio server too, in one frame.
    let request = json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"browser_screenshot","arguments":{}}});
    let mut output = Vec::new();
    mcp_serve(
        format!("{request}\n").as_bytes(),
        &mut output,
        &stage.environment,
    )
    .unwrap();
    let frame: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(frame["id"], 5);
    assert_eq!(
        frame["result"]["content"][1]["data"]
            .as_str()
            .unwrap()
            .len(),
        data.len()
    );
}

#[test]
fn a_token_opens_its_own_session_and_nothing_else() {
    let stage = Stage::new(Pump::Running, slow);
    // A tool the bridge does not know is refused before anything runs.
    assert_eq!(
        said(&stage.call("browser_everything", json!({}))),
        ("Unknown browser tool".into(), true)
    );
    // The same session keeps its token and follows its newest project.
    let again = stage
        .hub
        .environment(
            SESSION,
            Path::new("/work/other"),
            Path::new("/bin/monocode"),
        )
        .unwrap();
    assert_eq!(again[ENV_TOKEN], stage.environment[ENV_TOKEN]);
    assert_eq!(
        stage.hub.admit(SESSION, &again[ENV_TOKEN]).unwrap(),
        Scope {
            session: SESSION,
            project: "/work/other".into()
        }
    );
    let other = stage
        .hub
        .environment(42, Path::new(PROJECT), Path::new("/bin/monocode"))
        .unwrap();
    assert_ne!(other[ENV_TOKEN], stage.environment[ENV_TOKEN]);
    let refused = |environment: &HashMap<String, String>| {
        let answer = forward(environment, "browser_echo", json!({})).unwrap();
        assert_eq!(said(&answer), ("Invalid browser capability".into(), true));
    };
    // One session's token does not open another, nor a session nobody
    // started, nor a token cut short.
    let mut stolen = other.clone();
    stolen.insert(ENV_SESSION.into(), SESSION.to_string());
    refused(&stolen);
    let mut unknown = stage.environment.clone();
    unknown.insert(ENV_SESSION.into(), "9999".into());
    refused(&unknown);
    let mut short = stage.environment.clone();
    short.insert(ENV_TOKEN.into(), String::new());
    refused(&short);
    let mut wrong = stage.environment.clone();
    let mut token = wrong[ENV_TOKEN].clone();
    let last = if token.ends_with('0') { "1" } else { "0" };
    token.replace_range(token.len() - 1.., last);
    wrong.insert(ENV_TOKEN.into(), token);
    refused(&wrong);
    assert!(forward(&HashMap::new(), "browser_echo", json!({})).is_err());
    assert!(stage.hub.endpoint.starts_with("127.0.0.1:"));
}

#[test]
fn every_listed_tool_is_allowed_but_those_the_user_approves() {
    let definitions: Vec<Value> = ["browser_tab_list", "browser_eval", "browser_click"]
        .iter()
        .map(|name| json!({"name": name}))
        .collect();
    assert_eq!(
        allowed_tools(&definitions),
        "mcp__monocode_browser__browser_tab_list,mcp__monocode_browser__browser_click"
    );
    assert!(tools::NEEDS_APPROVAL.contains(&"browser_eval"));
    assert!(!claude_allowed_tools().contains("browser_eval"));
    // The list comes from the tools without the app.
    let listed = mcp_response(json!({"id":1,"method":"tools/list"}), &HashMap::new()).unwrap();
    assert_eq!(listed["result"]["tools"], json!(tools::definitions()));
}

#[test]
fn providers_are_handed_the_stdio_server() {
    let environment = HashMap::from([
        (ENV_ENDPOINT.to_owned(), "127.0.0.1:9".to_owned()),
        (ENV_TOKEN.to_owned(), "secret".to_owned()),
        (ENV_SESSION.to_owned(), "7".to_owned()),
        (ENV_EXECUTABLE.to_owned(), "/Apps/Monocode".to_owned()),
    ]);
    assert_eq!(
        mcp_server(&environment).unwrap(),
        json!({"type":"stdio","command":"/Apps/Monocode","args":["browser-mcp"]})
    );
    let acp = acp_server(&environment).unwrap();
    assert_eq!(acp["name"], SERVER_NAME);
    assert!(acp.get("type").is_none());
    assert_eq!(acp["env"].as_array().unwrap().len(), 4);
    let mut command = std::process::Command::new("codex");
    configure_codex(&mut command, &environment);
    let arguments: Vec<String> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert!(arguments.contains(&r#"mcp_servers.monocode_browser.args=["browser-mcp"]"#.to_owned()));
    // The token travels in the environment, never on a command line.
    assert!(!arguments.iter().any(|argument| argument.contains("secret")));
    assert!(mcp_server(&HashMap::new()).is_none());
    assert!(acp_server(&HashMap::new()).is_none());
}

#[test]
fn the_stdio_server_speaks_one_json_frame_per_line() {
    let serve = |input: &str| {
        let mut output = Vec::new();
        let result = mcp_serve(input.as_bytes(), &mut output, &HashMap::new());
        let frames: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (result, frames)
    };
    let (result, frames) = serve(concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        "not json\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":"four","method":"tools/call","params":{"name":"browser_tab_list","arguments":{}}}"#,
        "\n",
    ));
    assert_eq!(result, Ok(()));
    // The notification gets no frame; everything else gets exactly one.
    assert_eq!(frames.len(), 5);
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["serverInfo"]["name"], SERVER_NAME);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(frames[0]["result"]["instructions"], tools::INSTRUCTIONS);
    assert_eq!(frames[1]["error"]["code"], -32700);
    assert_eq!(frames[2]["result"], json!({}));
    assert_eq!(frames[3]["error"]["code"], -32601);
    // Outside a session a call explains itself instead of hanging.
    assert_eq!(frames[4]["id"], "four");
    assert_eq!(frames[4]["result"]["isError"], true);
    assert_eq!(
        frames[4]["result"]["content"][0]["text"],
        "The browser tools are only available inside a Monocode session"
    );
    // A frame cut off before its line end, or longer than a frame may be,
    // ends the server rather than being guessed at.
    let (result, frames) = serve(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    assert!(result.is_err());
    assert!(frames.is_empty());
    let huge = format!("{}\n", "x".repeat(MAX_WIRE_BYTES + 10));
    assert!(serve(&huge).0.is_err());
    assert_eq!(serve("").0, Ok(()));
}

#[test]
fn answers_are_cut_to_what_a_model_can_take() {
    let long = "ş".repeat(MAX_TEXT_BYTES);
    let answer = call_result(ToolOutput {
        content: vec![
            Content::Text(long),
            Content::Image {
                mime: "image/png".into(),
                data: "x".repeat(MAX_IMAGE_BYTES + 1),
            },
        ],
        is_error: false,
    });
    let text = answer["content"][0]["text"].as_str().unwrap();
    assert!(text.len() < MAX_TEXT_BYTES + 80);
    assert!(text.ends_with(" more bytes. Ask for less at a time.]"));
    // An image too large to send is said so, not sent.
    assert_eq!(answer["content"][1]["type"], "text");
    assert!(
        answer["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("too large"),
        "{}",
        answer["content"][1]
    );
    assert_eq!(answer["isError"], false);
}

#[test]
fn the_apps_tools_run_on_the_pump_with_its_host() {
    // The real tools, whatever they answer, answer a well-formed result
    // through the same path as the app's.
    let app = Engine::app();
    let stage = Stage::with_run(Pump::Running, slow, app.run, |_| true);
    let answer = stage.call("browser_tab_list", json!({}));
    assert!(
        !answer["content"].as_array().unwrap().is_empty(),
        "{answer}"
    );
    assert!(answer["isError"].is_boolean(), "{answer}");
}
