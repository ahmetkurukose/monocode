//! One request, start to finish: pre-request scripts, variables and auth,
//! the network call, then test scripts. Blocking and self-contained, so a
//! view can run it on a thread and apply [`ExchangeOutput`] when it is back.

use super::model::{ApiRequest, Auth, Body, KeyValue, RawLanguage};
use super::prepare::{prepare, Prepared};
use super::script::{self, ScriptRequest, ScriptResponse, ScriptVars, TestResult};
use super::send::{send, Response, SendOptions};
use super::vars::Scope;
use super::ApiError;

#[derive(Debug, Clone)]
pub struct ExchangeInput {
    pub request: ApiRequest,
    /// What the folders and collection give a request with no auth of its own.
    pub inherited_auth: Auth,
    /// The collection's variables with its folders' on top.
    pub collection_vars: Vec<KeyValue>,
    pub environment_name: String,
    pub environment_vars: Vec<KeyValue>,
    pub globals: Vec<KeyValue>,
    /// Pre-request scripts, outermost first, the request's own last.
    pub pre_scripts: Vec<String>,
    pub test_scripts: Vec<String>,
    pub options: SendOptions,
}

/// A variable a script set (`Some`) or removed (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarChange {
    pub key: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExchangeOutput {
    pub outcome: Result<Response, ApiError>,
    /// The request as sent, after the pre-request scripts changed it.
    pub request: ApiRequest,
    pub prepared: Option<Prepared>,
    pub tests: Vec<TestResult>,
    pub logs: Vec<String>,
    pub environment: Vec<VarChange>,
    pub collection: Vec<VarChange>,
    pub globals: Vec<VarChange>,
}

fn enabled(rows: &[KeyValue]) -> Vec<KeyValue> {
    rows.iter()
        .filter(|row| !row.disabled && !row.key.is_empty())
        .cloned()
        .collect()
}

/// What `after` did to `before`.
pub fn changes(before: &[KeyValue], after: &[KeyValue]) -> Vec<VarChange> {
    let before = enabled(before);
    let mut out = Vec::new();
    for row in after {
        if !before
            .iter()
            .any(|b| b.key == row.key && b.value == row.value)
        {
            out.push(VarChange {
                key: row.key.clone(),
                value: Some(row.value.clone()),
            });
        }
    }
    for row in &before {
        if !after.iter().any(|a| a.key == row.key) {
            out.push(VarChange {
                key: row.key.clone(),
                value: None,
            });
        }
    }
    out
}

fn script_request(request: &ApiRequest) -> ScriptRequest {
    ScriptRequest {
        method: request.method.clone(),
        url: request.url_text(),
        headers: request
            .headers
            .iter()
            .filter(|h| !h.disabled && !h.key.trim().is_empty())
            .map(|h| (h.key.clone(), h.value.clone()))
            .collect(),
        body: match &request.body {
            Body::Raw { text, .. } => Some(text.clone()),
            _ => None,
        },
    }
}

/// Puts what a script changed on the request back onto it.
fn apply_request(request: &mut ApiRequest, before: &ScriptRequest, after: &ScriptRequest) {
    if after.method != before.method {
        request.method = after.method.to_uppercase();
    }
    if after.url != before.url {
        request.set_url_text(&after.url);
    }
    if after.headers != before.headers {
        let disabled: Vec<KeyValue> = request
            .headers
            .iter()
            .filter(|h| h.disabled)
            .cloned()
            .collect();
        request.headers = after
            .headers
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
            .chain(disabled)
            .collect();
    }
    if let (true, Some(text)) = (after.body != before.body, &after.body) {
        let language = match &request.body {
            Body::Raw { language, .. } => *language,
            _ if text.trim_start().starts_with(['{', '[']) => RawLanguage::Json,
            _ => RawLanguage::Text,
        };
        request.body = Body::Raw {
            text: text.clone(),
            language,
        };
    }
}

pub fn run(input: ExchangeInput) -> ExchangeOutput {
    let ExchangeInput {
        mut request,
        inherited_auth,
        collection_vars,
        environment_name,
        environment_vars,
        globals,
        pre_scripts,
        test_scripts,
        options,
    } = input;
    let start = ScriptVars {
        environment: environment_vars,
        collection: collection_vars,
        globals,
        local: Vec::new(),
    };
    let mut vars = start.clone();
    let mut logs = Vec::new();
    let name = request.name.clone();

    let before = script_request(&request);
    let pre = script::run(
        "prerequest",
        &name,
        &environment_name,
        &pre_scripts,
        vars.clone(),
        before.clone(),
        None,
    );
    logs.extend(pre.logs);
    vars = pre.vars;
    apply_request(&mut request, &before, &pre.request);
    let finish = |outcome, request, prepared, tests, logs, vars: &ScriptVars| ExchangeOutput {
        outcome,
        request,
        prepared,
        tests,
        logs,
        environment: changes(&start.environment, &vars.environment),
        collection: changes(&start.collection, &vars.collection),
        globals: changes(&start.globals, &vars.globals),
    };
    if let Some(error) = pre.error {
        return finish(
            Err(ApiError::new(format!("Pre-request script failed: {error}"))),
            request,
            None,
            Vec::new(),
            logs,
            &vars,
        );
    }

    let mut scope = Scope::new();
    scope.push(vars.globals.clone());
    scope.push(vars.collection.clone());
    scope.push(vars.environment.clone());
    scope.push(vars.local.clone());
    let auth = match &request.auth {
        Auth::Inherit => inherited_auth,
        own => own.clone(),
    };
    let prepared = prepare(&request, &auth, &scope);
    let outcome = send(&prepared, &options);

    let mut tests = Vec::new();
    let wants_tests = test_scripts.iter().any(|s| !s.trim().is_empty());
    if let (Ok(response), true) = (&outcome, wants_tests) {
        let sent = script_request(&request);
        let run = script::run(
            "test",
            &name,
            &environment_name,
            &test_scripts,
            vars.clone(),
            sent,
            Some(ScriptResponse {
                code: response.status,
                status: response.reason.clone(),
                headers: response.headers.clone(),
                body: response.text(),
                time_ms: response.elapsed.as_millis() as u64,
                size: response.body.len(),
            }),
        );
        logs.extend(run.logs);
        vars = run.vars;
        tests = run.tests;
        if let Some(error) = run.error {
            tests.push(TestResult {
                name: "Script error".into(),
                passed: false,
                error: Some(error),
                skipped: false,
            });
        }
    }
    finish(outcome, request, Some(prepared), tests, logs, &vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serves `count` canned answers and returns the raw requests it saw.
    fn serve(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut received = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    received.extend_from_slice(&chunk[..n]);
                    if n == 0 || received.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                stream.write_all(reply.as_bytes()).unwrap();
                seen.push(String::from_utf8_lossy(&received).into_owned());
            }
            seen
        });
        (url, handle)
    }

    const OK_JSON: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"token\":\"t0k\",\"n\":42}";

    fn input(url: &str) -> ExchangeInput {
        let mut request = ApiRequest {
            name: "Login".into(),
            ..ApiRequest::default()
        };
        request.set_url_text(url);
        ExchangeInput {
            request,
            inherited_auth: Auth::None,
            collection_vars: vec![KeyValue::new("page", "1")],
            environment_name: "local".into(),
            environment_vars: Vec::new(),
            globals: Vec::new(),
            pre_scripts: Vec::new(),
            test_scripts: Vec::new(),
            options: SendOptions::default(),
        }
    }

    #[test]
    fn a_login_sets_a_token_the_next_request_can_use() {
        let (url, server) = serve(vec![OK_JSON]);
        let mut login = input(&format!("{url}/login?p={{{{page}}}}"));
        login.test_scripts = vec!["pm.test('ok', () => pm.response.to.have.status(200)); \
             pm.environment.set('token', pm.response.json().token); \
             pm.collectionVariables.set('page', '2');"
            .into()];
        let out = run(login);
        assert_eq!(out.outcome.as_ref().unwrap().status, 200);
        assert_eq!(out.tests.len(), 1);
        assert!(out.tests[0].passed);
        assert_eq!(
            out.environment,
            vec![VarChange {
                key: "token".into(),
                value: Some("t0k".into())
            }]
        );
        assert_eq!(
            out.collection,
            vec![VarChange {
                key: "page".into(),
                value: Some("2".into())
            }]
        );
        let seen = server.join().unwrap();
        assert!(
            seen[0].starts_with("GET /login?p=1 HTTP/1.1"),
            "{}",
            seen[0]
        );

        // The next request picks the token up from the environment.
        let (url, server) = serve(vec![OK_JSON]);
        let mut next = input(&format!("{url}/me"));
        next.environment_vars = vec![KeyValue::new("token", "t0k")];
        next.request.auth = Auth::Bearer {
            token: "{{token}}".into(),
        };
        let out = run(next);
        assert!(out.outcome.is_ok());
        assert!(server.join().unwrap()[0].contains("Authorization: Bearer t0k"));
    }

    #[test]
    fn a_pre_request_script_can_change_what_is_sent() {
        let (url, server) = serve(vec![OK_JSON]);
        let mut req = input(&format!("{url}/items"));
        req.pre_scripts = vec!["pm.variables.set('stamp', 'abc'); \
             pm.request.headers.add({key: 'X-Stamp', value: pm.variables.get('stamp')}); \
             pm.request.url = pm.request.url.toString() + '?s={{stamp}}'; \
             pm.request.method = 'delete';"
            .into()];
        let out = run(req);
        assert!(out.outcome.is_ok());
        assert_eq!(out.request.method, "DELETE");
        let seen = server.join().unwrap();
        assert!(
            seen[0].starts_with("DELETE /items?s=abc HTTP/1.1"),
            "{}",
            seen[0]
        );
        assert!(seen[0].contains("X-Stamp: abc"));
        // Local variables never leave the run.
        assert!(out.environment.is_empty() && out.collection.is_empty());
    }

    #[test]
    fn a_failing_pre_request_script_stops_the_request() {
        let mut req = input("http://127.0.0.1:1/never");
        req.pre_scripts = vec!["console.log('ran'); throw new Error('boom');".into()];
        let out = run(req);
        let error = out.outcome.unwrap_err().to_string();
        assert!(
            error.contains("Pre-request script failed") && error.contains("boom"),
            "{error}"
        );
        assert!(out.prepared.is_none());
        assert_eq!(out.logs, vec!["ran"]);
    }

    #[test]
    fn a_broken_test_script_is_reported_as_a_failed_test_not_a_failed_request() {
        let (url, _server) = serve(vec![OK_JSON]);
        let mut req = input(&format!("{url}/x"));
        req.test_scripts = vec!["pm.test('fine', () => {}); undefinedFunction();".into()];
        let out = run(req);
        assert!(out.outcome.is_ok());
        assert_eq!(out.tests.len(), 2);
        assert!(out.tests[0].passed && !out.tests[1].passed);
        assert!(out.tests[1]
            .error
            .as_deref()
            .unwrap()
            .contains("ReferenceError"));
    }

    #[test]
    fn tests_do_not_run_when_there_is_no_response() {
        let mut req = input("http://127.0.0.1:1/down");
        req.test_scripts = vec!["pm.test('never', () => {});".into()];
        let out = run(req);
        assert!(out.outcome.is_err() && out.tests.is_empty());
    }

    #[test]
    fn variable_changes_are_diffed_against_the_enabled_variables() {
        let before = vec![
            KeyValue::new("a", "1"),
            KeyValue::new("b", "2"),
            KeyValue {
                key: "off".into(),
                value: "x".into(),
                disabled: true,
            },
        ];
        let after = vec![
            KeyValue::new("a", "1"),
            KeyValue::new("b", "3"),
            KeyValue::new("c", "4"),
        ];
        assert_eq!(
            changes(&before, &after),
            vec![
                VarChange {
                    key: "b".into(),
                    value: Some("3".into())
                },
                VarChange {
                    key: "c".into(),
                    value: Some("4".into())
                },
            ]
        );
        assert_eq!(
            changes(&before, &[KeyValue::new("a", "1")]),
            vec![VarChange {
                key: "b".into(),
                value: None
            }]
        );
    }
}
