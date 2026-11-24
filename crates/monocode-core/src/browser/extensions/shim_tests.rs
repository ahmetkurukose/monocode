//! The shim in QuickJS, over a scripted transport: every asynchronous
//! method of every context calls its callback (once, `lastError` set on
//! failure) and returns a promise without one. A method that ignored its
//! callback would hang an extension that uses the webextension polyfill.

use rquickjs::{Context, Function, Runtime};
use serde_json::{Value, json};

use super::{manifest::Manifest, scripts::SHIM};

/// Methods that answer at once (no callback, no promise) in Chrome too.
const SYNC: &[&str] = &[
    "runtime.getURL",
    "runtime.getManifest",
    "runtime.connect",
    "runtime.connectNative",
    "runtime.reload",
    "i18n.getMessage",
    "i18n.getUILanguage",
    "extension.getURL",
    "extension.getViews",
    "extension.getBackgroundPage",
    "extension.setUpdateUrlData",
    "tabs.connect",
    "idle.setDetectionInterval",
    "identity.getRedirectURL",
    // Returns the item's id; its optional callback runs later.
    "contextMenus.create",
];

const HARNESS: &str = r#"
globalThis.console = { log() {}, warn() {}, error() {}, debug() {} };
globalThis.setTimeout = (f) => { Promise.resolve().then(() => f()); return 0; };
globalThis.__answer = "ok";
globalThis.__calls = [];
globalThis.__inbox = null;
globalThis.__transport = {
  call(api, args) {
    __calls.push(api);
    if (api === "ctx.hello") return Promise.resolve(globalThis.__hello || {});
    if (api === "runtime.sendMessage" && globalThis.__noResponse) return Promise.resolve({ __mcextNone: true });
    return __answer === "ok" ? Promise.resolve(null) : Promise.reject(new Error("scripted failure"));
  },
  listen(f) { __inbox = f; },
};
globalThis.__walk = (root, sync) => {
  const out = [];
  const seen = new Set();
  const visit = (obj, path, depth) => {
    if (!obj || typeof obj !== "object" || seen.has(obj) || depth > 4) return;
    seen.add(obj);
    const isEvent = typeof obj.addListener === "function";
    for (const key of Object.keys(obj)) {
      const d = Object.getOwnPropertyDescriptor(obj, key);
      if (!d || !("value" in d)) continue;
      const name = path ? path + "." + key : key;
      if (typeof d.value === "function") {
        if (isEvent && ["addListener", "removeListener", "hasListener", "hasListeners"].includes(key)) continue;
        if (sync.includes(name)) continue;
        out.push([name, obj, key]);
      } else visit(d.value, name, depth + 1);
    }
  };
  visit(root, "", 0);
  return out;
};
globalThis.__check = (sync) => {
  const methods = __walk(globalThis.chrome, sync);
  const results = { methods: methods.length, noCallback: [], twice: [], noLastError: [], noPromise: [] };
  const pending = [];
  for (const [name, obj, key] of methods) {
    let n = 0;
    let sawError = false;
    const before = __calls.length;
    try {
      const r = obj[key]((x) => { n++; sawError = !!chrome.runtime.lastError; });
      if (r !== undefined) results.noPromise.push(name + " returned with a callback");
    } catch (e) { results.noCallback.push(name + " threw " + e.message); continue; }
    // Answered by the app (not locally): a failure must set lastError.
    const remote = __calls.length > before;
    pending.push(() => {
      if (n === 0) results.noCallback.push(name);
      if (n > 1) results.twice.push(name);
      if (__answer !== "ok" && remote && n === 1 && !sawError) results.noLastError.push(name);
    });
    let p;
    try { p = obj[key](); } catch (e) { p = null; }
    if (!p || typeof p.then !== "function") results.noPromise.push(name);
    else p.catch(() => {});
  }
  globalThis.__finish = () => { pending.forEach((f) => f()); return JSON.stringify(results); };
};
"#;

fn manifest() -> Manifest {
    Manifest::from_value(
        serde_json::from_str(super::manifest::PROTON_MANIFEST).unwrap(),
        serde_json::Map::new(),
    )
    .unwrap()
}

/// Runs the shim in `mode` with `answer`, then the check; the results.
fn check(mode: &str, answer: &str) -> Value {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let m = manifest();
    let cfg = json!({
        "id": super::PROTON_PASS_ID,
        "base": super::base_url(super::PROTON_PASS_ID),
        "manifest": m.raw,
        "messages": {"hello": "Hello $1"},
        "locale": "en-US",
        "mode": mode,
    });
    context.with(|ctx| {
        ctx.eval::<(), _>(HARNESS).unwrap();
        ctx.eval::<(), _>(format!(
            "globalThis.__MCEXT_CFG = {cfg}; __MCEXT_CFG.transport = __transport; __answer = {};",
            Value::String(answer.into())
        ))
        .unwrap();
        ctx.eval::<(), _>(SHIM).unwrap();
        let check: Function = ctx.globals().get("__check").unwrap();
        let sync: Vec<String> = SYNC.iter().map(|s| s.to_string()).collect();
        check.call::<_, ()>((sync,)).unwrap();
    });
    while runtime.execute_pending_job().unwrap_or(false) {}
    let out = context.with(|ctx| {
        let finish: Function = ctx.globals().get("__finish").unwrap();
        finish.call::<_, String>(()).unwrap()
    });
    serde_json::from_str(&out).unwrap()
}

fn assert_clean(results: &Value, mode: &str) {
    for key in ["noCallback", "twice", "noLastError", "noPromise"] {
        assert_eq!(results[key], json!([]), "{mode}: {key}: {results}");
    }
}

#[test]
fn every_page_method_calls_its_callback_and_returns_a_promise() {
    let ok = check("page", "ok");
    assert!(ok["methods"].as_u64().unwrap() > 150, "{ok}");
    assert_clean(&ok, "page ok");
    let failing = check("page", "fail");
    assert_clean(&failing, "page fail");
}

#[test]
fn every_content_script_method_calls_its_callback() {
    let ok = check("content", "ok");
    assert!(ok["methods"].as_u64().unwrap() > 10, "{ok}");
    assert_clean(&ok, "content ok");
    assert_clean(&check("content", "fail"), "content fail");
}

/// Runs `script` after the shim (mode page) and the pending jobs; the
/// value of `__result`.
fn run(script: &str, setup: &str) -> Value {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let m = manifest();
    let cfg = json!({"id": super::PROTON_PASS_ID, "base": super::base_url(super::PROTON_PASS_ID),
        "manifest": m.raw, "messages": {"hello": "Hello $1 and $2"}, "locale": "de-DE", "mode": "page"});
    context.with(|ctx| {
        ctx.eval::<(), _>(HARNESS).unwrap();
        ctx.eval::<(), _>(setup).unwrap();
        ctx.eval::<(), _>(format!(
            "globalThis.__MCEXT_CFG = {cfg}; __MCEXT_CFG.transport = __transport;"
        ))
        .unwrap();
        ctx.eval::<(), _>(SHIM).unwrap();
        ctx.eval::<(), _>(format!("globalThis.__result = null; {script}"))
            .unwrap();
    });
    while runtime.execute_pending_job().unwrap_or(false) {}
    context.with(|ctx| {
        let s: String = ctx.eval("JSON.stringify(globalThis.__result)").unwrap();
        serde_json::from_str(&s).unwrap()
    })
}

#[test]
fn send_message_without_a_response_differs_by_style() {
    // Callback: lastError "port closed" (the polyfill resolves undefined on
    // it); promise: undefined.
    let r = run(
        r#"__result = {};
        chrome.runtime.sendMessage({a: 1}, (v) => { __result.cb = [v === undefined, chrome.runtime.lastError && chrome.runtime.lastError.message]; });
        chrome.runtime.sendMessage({a: 1}).then((v) => { __result.promise = v === undefined; });"#,
        "globalThis.__noResponse = true;",
    );
    assert_eq!(r["cb"], json!([true, super::api::PORT_CLOSED]));
    assert_eq!(r["promise"], true);
}

#[test]
fn lifecycle_events_reach_late_listeners_and_sync_methods_answer() {
    let r = run(
        r#"__result = {};
        Promise.resolve().then(() => Promise.resolve()).then(() => {
          chrome.runtime.onInstalled.addListener((d) => { __result.installed = d; });
        });
        __result.url = chrome.runtime.getURL("/popup.html");
        __result.msg = chrome.i18n.getMessage("HELLO", ["a", "b"]);
        __result.id = chrome.i18n.getMessage("@@extension_id");
        __result.lang = chrome.i18n.getUILanguage();
        __result.version = chrome.runtime.getManifest().version;
        const p = chrome.runtime.connectNative("com.example");
        p.onDisconnect.addListener(() => { __result.native = chrome.runtime.lastError.message; });"#,
        r#"globalThis.__hello = {installed: "update", previousVersion: "1.43.0"};"#,
    );
    assert_eq!(
        r["installed"],
        json!({"reason": "update", "previousVersion": "1.43.0"})
    );
    assert_eq!(
        r["url"],
        format!("mcext://{}/popup.html", super::PROTON_PASS_ID)
    );
    assert_eq!(r["msg"], "Hello a and b");
    assert_eq!(r["id"], super::PROTON_PASS_ID);
    assert_eq!(r["lang"], "de-DE");
    assert_eq!(r["version"], "1.44.0");
    assert_eq!(r["native"], "Specified native messaging host not found.");
}

#[test]
fn events_dispatch_with_filters_and_ports_close() {
    let r = run(
        r#"__result = {hits: [], port: null};
        chrome.webRequest.onCompleted.addListener((d) => __result.hits.push(d.url), {urls: ["https://*.proton.me/*"], types: ["main_frame"]});
        chrome.storage.onChanged.addListener((c, area) => { __result.area = area; });
        chrome.runtime.onConnect.addListener((port) => {
          port.onMessage.addListener((m) => { __result.port = m; });
          port.onDisconnect.addListener(() => { __result.closed = true; });
        });
        __inbox({t: "event", name: "webRequest.onCompleted", filter: true, args: [{url: "https://account.proton.me/", type: "main_frame"}]});
        __inbox({t: "event", name: "webRequest.onCompleted", filter: true, args: [{url: "https://example.com/", type: "main_frame"}]});
        __inbox({t: "event", name: "webRequest.onCompleted", filter: true, args: [{url: "https://pass.proton.me/x", type: "xmlhttprequest"}]});
        __inbox({t: "event", name: "storage.onChanged", args: [{a: {newValue: 1}}, "local"]});
        __inbox({t: "connect", port: "p1", name: "x", sender: {id: "y"}});
        __inbox({t: "port.message", port: "p1", msg: {hi: 1}});
        __inbox({t: "port.disconnect", port: "p1"});"#,
        "",
    );
    assert_eq!(r["hits"], json!(["https://account.proton.me/"]));
    assert_eq!(r["area"], "local");
    assert_eq!(r["port"], json!({"hi": 1}));
    assert_eq!(r["closed"], true);
}

#[test]
fn content_programs_parse_and_carry_the_right_scripts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("orchestrator.js"),
        "globalThis.__ran = 'orchestrator';",
    )
    .unwrap();
    std::fs::write(dir.path().join("webauthn.js"), "window.__webauthn = 1;").unwrap();
    let m = manifest();
    let config = super::scripts::ShimConfig {
        id: super::PROTON_PASS_ID,
        manifest: &m,
        locale: "en-US",
        expose: false,
    };
    let names = super::scripts::Names::new(super::PROTON_PASS_ID, "s3cr3t");
    let (isolated, main) = super::scripts::content_programs(&config, dir.path(), &names);
    let (isolated, main) = (isolated.unwrap(), main.unwrap());
    assert!(isolated.contains("__ran = 'orchestrator'"));
    assert!(!isolated.contains("__webauthn"));
    assert!(main.contains("__webauthn"));
    // The main world never sees the shim or the binding.
    assert!(!main.contains("__monocodeExt"));
    assert!(!main.contains("x-mcext-token"));
    assert!(main.contains("__MCEXT_EXT"));
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let compile: Function = ctx
            .eval("(src) => { new Function(src); return true; }")
            .unwrap();
        assert!(compile.call::<_, bool>((isolated.clone(),)).unwrap());
        assert!(compile.call::<_, bool>((main.clone(),)).unwrap());
    });
}

#[test]
fn tabs_get_current_outside_a_tab_is_undefined_as_in_chrome() {
    // The app answers null outside a tab; Proton Pass's popup takes any
    // other value than undefined for "opened in a tab" and sizes to 100%.
    let r = run(
        r#"__result = {};
        chrome.tabs.getCurrent().then((t) => { __result.promise = t === undefined; });
        chrome.tabs.getCurrent((t) => { __result.cb = t === undefined; });"#,
        "",
    );
    assert_eq!(r["promise"], true);
    assert_eq!(r["cb"], true);
}
