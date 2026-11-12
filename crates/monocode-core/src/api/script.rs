//! Postman scripts: pre-request and test scripts run in an embedded QuickJS
//! against a `pm` object ([`script_sdk.js`]). A run is bounded in time and
//! memory and sees nothing of the host; variables, the request and the
//! response go in as JSON and the changes come back as JSON.

use super::model::KeyValue;
use rquickjs::{Context, Function, Runtime};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

const TIME_LIMIT: Duration = Duration::from_secs(8);
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const MAX_SCRIPT_BYTES: usize = 512 * 1024;
const MAX_LOGS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub skipped: bool,
}

/// The variable scopes a script can read and change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptVars {
    pub environment: Vec<KeyValue>,
    pub collection: Vec<KeyValue>,
    pub globals: Vec<KeyValue>,
    /// `pm.variables.set`: they last for one request and win over the rest.
    pub local: Vec<KeyValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRequest {
    pub method: String,
    /// The URL as typed, query included, variables not yet filled in.
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The raw body, when the request has one.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptResponse {
    pub code: u16,
    pub status: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub time_ms: u64,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct ScriptRun {
    pub vars: ScriptVars,
    pub request: ScriptRequest,
    pub tests: Vec<TestResult>,
    pub logs: Vec<String>,
    /// Why a script stopped early. Later scripts did not run.
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct Output {
    environment: Vec<(String, String)>,
    collection: Vec<(String, String)>,
    globals: Vec<(String, String)>,
    local: Vec<(String, String)>,
    request: OutputRequest,
    tests: Vec<TestResult>,
    logs: Vec<String>,
}

#[derive(Deserialize)]
struct OutputRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

fn pairs(rows: &[KeyValue]) -> Vec<(&str, &str)> {
    rows.iter()
        .filter(|row| !row.disabled && !row.key.is_empty())
        .map(|row| (row.key.as_str(), row.value.as_str()))
        .collect()
}

fn rows(pairs: Vec<(String, String)>) -> Vec<KeyValue> {
    pairs
        .into_iter()
        .map(|(key, value)| KeyValue::new(key, value))
        .collect()
}

/// Runs `scripts` one after another in a single context, so they share state
/// as Postman's sandbox does. `event` is `"prerequest"` or `"test"`.
pub fn run(
    event: &str,
    request_name: &str,
    environment_name: &str,
    scripts: &[String],
    vars: ScriptVars,
    request: ScriptRequest,
    response: Option<ScriptResponse>,
) -> ScriptRun {
    let unchanged = |vars, request, error: Option<String>| ScriptRun {
        vars,
        request,
        tests: Vec::new(),
        logs: Vec::new(),
        error,
    };
    let scripts: Vec<&String> = scripts.iter().filter(|s| !s.trim().is_empty()).collect();
    if scripts.is_empty() {
        return unchanged(vars, request, None);
    }
    if scripts.iter().any(|s| s.len() > MAX_SCRIPT_BYTES) {
        return unchanged(
            vars,
            request,
            Some("A script is larger than 512 KB.".into()),
        );
    }
    let input = serde_json::json!({
        "event": event,
        "requestName": request_name,
        "environmentName": environment_name,
        "environment": pairs(&vars.environment),
        "collection": pairs(&vars.collection),
        "globals": pairs(&vars.globals),
        "local": pairs(&vars.local),
        "request": {
            "method": request.method,
            "url": request.url,
            "headers": request.headers,
            "body": request.body,
        },
        "response": response.as_ref().map(|r| serde_json::json!({
            "code": r.code,
            "status": r.status,
            "headers": r.headers,
            "body": r.body,
            "timeMs": r.time_ms,
            "size": r.size,
        })),
    })
    .to_string();

    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            return unchanged(
                vars,
                request,
                Some(format!("Could not start scripts: {error}")),
            );
        }
    };
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + TIME_LIMIT;
    let timed_out = Rc::new(Cell::new(false));
    let flag = timed_out.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let expired = Instant::now() >= deadline;
        if expired {
            flag.set(true);
        }
        expired
    })));
    let context = match Context::full(&runtime) {
        Ok(context) => context,
        Err(error) => {
            return unchanged(
                vars,
                request,
                Some(format!("Could not start scripts: {error}")),
            );
        }
    };

    let mut error: Option<String> = None;
    let setup = context.with(|ctx| -> rquickjs::Result<()> {
        ctx.globals().set("__in", input.as_str())?;
        ctx.eval::<(), _>(include_str!("script_sdk.js"))?;
        let exec: Function = ctx.globals().get("__exec")?;
        for script in &scripts {
            let failed: Option<String> = exec.call((script.as_str(),))?;
            if failed.is_some() {
                error = failed;
                break;
            }
        }
        Ok(())
    });
    if let Err(failure) = setup {
        let message = if timed_out.get() {
            format!(
                "The script ran longer than {} seconds and was stopped.",
                TIME_LIMIT.as_secs()
            )
        } else {
            format!("The script could not run: {failure}")
        };
        return unchanged(vars, request, Some(message));
    }
    // Promises from async tests settle here.
    for _ in 0..10_000 {
        match runtime.execute_pending_job() {
            Ok(true) => {}
            _ => break,
        }
    }
    let json = context.with(|ctx| -> rquickjs::Result<String> {
        let out: Function = ctx.globals().get("__out")?;
        out.call(())
    });
    let output: Option<Output> = json.ok().and_then(|json| serde_json::from_str(&json).ok());
    let Some(mut output) = output else {
        let message = if timed_out.get() {
            "The script ran longer than its time limit and was stopped.".to_owned()
        } else {
            "The script left its state in a form that could not be read.".to_owned()
        };
        return unchanged(vars, request, Some(error.unwrap_or(message)));
    };
    output.logs.truncate(MAX_LOGS);
    ScriptRun {
        vars: ScriptVars {
            environment: rows(output.environment),
            collection: rows(output.collection),
            globals: rows(output.globals),
            local: rows(output.local),
        },
        request: ScriptRequest {
            method: output.request.method,
            url: output.request.url,
            headers: output.request.headers,
            body: output.request.body,
        },
        tests: output.tests,
        logs: output.logs,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ScriptRequest {
        ScriptRequest {
            method: "GET".into(),
            url: "{{base}}/items?x=1".into(),
            headers: vec![("Accept".into(), "*/*".into())],
            body: None,
        }
    }

    fn vars() -> ScriptVars {
        ScriptVars {
            environment: vec![KeyValue::new("base", "http://h.test")],
            collection: vec![KeyValue::new("page", "1")],
            globals: Vec::new(),
            local: Vec::new(),
        }
    }

    fn pre(script: &str) -> ScriptRun {
        run(
            "prerequest",
            "Req",
            "env",
            &[script.to_owned()],
            vars(),
            request(),
            None,
        )
    }

    fn response(code: u16, body: &str) -> ScriptResponse {
        ScriptResponse {
            code,
            status: if code == 200 {
                "OK".into()
            } else {
                "Not Found".into()
            },
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            time_ms: 12,
            size: body.len(),
        }
    }

    fn tests(script: &str, code: u16, body: &str) -> ScriptRun {
        run(
            "test",
            "Req",
            "env",
            &[script.to_owned()],
            vars(),
            request(),
            Some(response(code, body)),
        )
    }

    fn value<'a>(rows: &'a [KeyValue], key: &str) -> Option<&'a str> {
        rows.iter().find(|r| r.key == key).map(|r| r.value.as_str())
    }

    #[test]
    fn no_scripts_cost_nothing_and_change_nothing() {
        let run = run("prerequest", "R", "e", &[], vars(), request(), None);
        assert_eq!(run.request, request());
        assert!(run.error.is_none() && run.tests.is_empty());
        let blank = self::run("test", "R", "e", &["  \n".into()], vars(), request(), None);
        assert!(blank.error.is_none());
    }

    #[test]
    fn variables_are_read_set_and_unset_in_their_own_scope() {
        let run = pre(r#"
            pm.environment.set("token", "abc");
            pm.environment.unset("base");
            pm.collectionVariables.set("page", 2);
            pm.globals.set("g", {a: 1});
            pm.variables.set("local", "x");
            console.log(pm.variables.get("page"), pm.variables.get("local"), pm.environment.get("nope"));
            console.log(pm.variables.replaceIn("{{page}}/{{token}}/{{missing}}"));
            "#);
        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(value(&run.vars.environment, "token"), Some("abc"));
        assert_eq!(value(&run.vars.environment, "base"), None);
        assert_eq!(value(&run.vars.collection, "page"), Some("2"));
        assert_eq!(value(&run.vars.globals, "g"), Some("{\"a\":1}"));
        assert_eq!(run.logs, vec!["2 x undefined", "2/abc/{{missing}}"]);
        assert_eq!(value(&run.vars.local, "local"), Some("x"));
    }

    #[test]
    fn the_request_can_be_changed_before_it_is_sent() {
        let run = pre(r#"
            pm.request.headers.add({key: "X-Trace", value: "1"});
            pm.request.headers.upsert({key: "accept", value: "application/json"});
            pm.request.headers.remove("nothing");
            pm.request.method = "POST";
            pm.request.url = pm.request.url.toString() + "&y=2";
            pm.request.body.raw = "hi";
            "#);
        assert!(run.error.is_none());
        assert_eq!(run.request.method, "POST");
        assert_eq!(run.request.url, "{{base}}/items?x=1&y=2");
        assert_eq!(
            run.request.headers,
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("X-Trace".to_owned(), "1".to_owned())
            ]
        );
        // Assigning a raw body gives the request one.
        assert_eq!(run.request.body.as_deref(), Some("hi"));
    }

    #[test]
    fn body_update_sets_a_raw_body() {
        let run = pre("pm.request.body.update({mode: 'raw', raw: JSON.stringify({a: 1})});");
        assert_eq!(run.request.body.as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn a_raw_body_can_be_read_and_replaced() {
        let mut req = request();
        req.body = Some("{\"a\":1}".into());
        let run = run(
            "prerequest",
            "R",
            "e",
            &["pm.request.body.raw = pm.request.body.raw.replace('1', '2');".into()],
            vars(),
            req,
            None,
        );
        assert_eq!(run.request.body.as_deref(), Some("{\"a\":2}"));
    }

    #[test]
    fn tests_record_passes_and_failures_with_reasons() {
        let run = tests(
            r#"
            pm.test("status is 200", () => pm.response.to.have.status(200));
            pm.test("has a name", () => {
                const j = pm.response.json();
                pm.expect(j.name).to.equal("ann");
                pm.expect(j.tags).to.include("a").and.have.lengthOf(2);
                pm.expect(j).to.have.property("age").that.is.above(17);
            });
            pm.test("wrong", () => pm.expect(1).to.equal(2));
            pm.test("negated", () => pm.expect("abc").to.not.include("z"));
            pm.test.skip("later");
            "#,
            200,
            r#"{"name":"ann","tags":["a","b"],"age":30}"#,
        );
        assert!(run.error.is_none(), "{:?}", run.error);
        let names: Vec<(&str, bool)> = run
            .tests
            .iter()
            .map(|t| (t.name.as_str(), t.passed))
            .collect();
        assert_eq!(
            names,
            vec![
                ("status is 200", true),
                ("has a name", true),
                ("wrong", false),
                ("negated", true),
                ("later", true)
            ]
        );
        assert_eq!(run.tests[2].error.as_deref(), Some("expected 1 to equal 2"));
        assert!(run.tests[4].skipped);
    }

    #[test]
    fn response_helpers_and_status_checks_work() {
        let run = tests(
            r#"
            pm.test("code", () => { pm.expect(pm.response.code).to.eql(404); });
            pm.test("text", () => pm.expect(pm.response.text()).to.include("gone"));
            pm.test("header", () => pm.response.to.have.header("content-type", "application/json"));
            pm.test("time", () => pm.expect(pm.response.responseTime).to.be.below(100));
            pm.test("clientError", () => pm.response.to.be.clientError);
            pm.test("not ok", () => pm.response.to.be.ok);
            pm.test("json type", () => pm.expect(pm.response.json()).to.be.an("object"));
            "#,
            404,
            r#"{"error":"gone"}"#,
        );
        let results: Vec<bool> = run.tests.iter().map(|t| t.passed).collect();
        assert_eq!(results, vec![true, true, true, true, true, false, true]);
        assert!(run.tests[5].error.as_deref().unwrap().contains("ok (200)"));
    }

    #[test]
    fn the_token_flow_that_logins_use_works() {
        let run = tests(
            r#"
            const body = pm.response.json();
            pm.environment.set("token", body.data.token);
            pm.collectionVariables.set("userId", body.data.id);
            "#,
            200,
            r#"{"data":{"token":"t0k","id":7}}"#,
        );
        assert_eq!(value(&run.vars.environment, "token"), Some("t0k"));
        assert_eq!(value(&run.vars.collection, "userId"), Some("7"));
    }

    #[test]
    fn legacy_postman_globals_still_work() {
        let run = tests(
            r#"
            tests["Status code is 200"] = responseCode.code === 200;
            tests["body"] = responseBody.indexOf("ok") >= 0;
            postman.setEnvironmentVariable("legacy", JSON.parse(responseBody).v);
            postman.setGlobalVariable("g", "1");
            "#,
            200,
            r#"{"v":"ok"}"#,
        );
        assert!(run.tests.iter().all(|t| t.passed) && run.tests.len() == 2);
        assert_eq!(value(&run.vars.environment, "legacy"), Some("ok"));
        assert_eq!(value(&run.vars.globals, "g"), Some("1"));
    }

    #[test]
    fn async_tests_settle_and_base64_helpers_exist() {
        let run = tests(
            r#"
            pm.test("later", async () => { await Promise.resolve(); throw new Error("late failure"); });
            console.log(btoa("hello:world"), atob("aGVsbG86d29ybGQ="));
            "#,
            200,
            "{}",
        );
        assert!(!run.tests[0].passed);
        assert_eq!(run.tests[0].error.as_deref(), Some("late failure"));
        assert_eq!(run.logs, vec!["aGVsbG86d29ybGQ= hello:world"]);
    }

    #[test]
    fn an_error_stops_the_scripts_but_keeps_what_ran() {
        let run = run(
            "prerequest",
            "R",
            "e",
            &[
                "pm.environment.set('a','1'); console.log('first'); nope();".into(),
                "pm.environment.set('never','1');".into(),
            ],
            vars(),
            request(),
            None,
        );
        let message = run.error.unwrap();
        assert!(message.contains("ReferenceError"), "{message}");
        assert_eq!(value(&run.vars.environment, "a"), Some("1"));
        assert_eq!(value(&run.vars.environment, "never"), None);
        assert_eq!(run.logs, vec!["first"]);
    }

    #[test]
    fn unsupported_things_say_so() {
        let run = pre("const crypto = require('crypto-js');");
        assert!(run
            .error
            .unwrap()
            .contains("require('crypto-js') is not supported"));
        let run = pre("pm.sendRequest('http://x', () => {});");
        assert!(run
            .error
            .unwrap()
            .contains("pm.sendRequest is not supported"));
    }

    #[test]
    fn several_scripts_share_one_context_in_order() {
        let run = run(
            "prerequest",
            "R",
            "e",
            &[
                "var shared = 5; pm.variables.set('n', shared);".into(),
                "pm.environment.set('seen', pm.variables.get('n') + ':' + shared);".into(),
            ],
            vars(),
            request(),
            None,
        );
        assert!(run.error.is_none());
        assert_eq!(value(&run.vars.environment, "seen"), Some("5:5"));
    }

    #[test]
    fn a_runaway_script_is_stopped() {
        let started = Instant::now();
        let run = pre("while (true) {}");
        assert!(run.error.unwrap().contains("longer than"));
        assert!(started.elapsed() < Duration::from_secs(20));
        // The state it started with comes back untouched.
        assert_eq!(run.vars, vars());
    }

    #[test]
    fn a_script_cannot_reach_the_host() {
        let run = pre("console.log(typeof process, typeof fetch, typeof XMLHttpRequest, typeof std, typeof os);");
        assert_eq!(
            run.logs,
            vec!["undefined undefined undefined undefined undefined"]
        );
    }
}
