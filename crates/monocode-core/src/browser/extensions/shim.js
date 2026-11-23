// The `chrome.*` API of one extension (docs/native/browser/context.md K69),
// bridged to Monocode. It runs first in every context of the extension,
// configured by `globalThis.__MCEXT_CFG` (see scripts.rs):
//
// - mode "page": extension pages (popup, options, tabs, iframes in web
//   pages, offscreen documents) and the background worker. Calls are
//   POSTs to `<base>__mcext/api`, events come from a long poll of
//   `<base>__mcext/events`; both carry the instance's token.
// - mode "content": content scripts, in the extension's isolated world of
//   a web page. Calls and events go through a CDP binding.
// - mode "external": the relay of a web page the manifest's
//   `externally_connectable` matches (isolated world); its main world gets
//   `chrome.runtime.sendMessage/connect` from `external.js`.
//
// Every asynchronous method goes through one adapter (`asyncMethod`): it
// returns a promise when called without a callback, and with a trailing
// callback calls it once, with `runtime.lastError` set on failure. The
// webextension polyfill (Proton Pass) passes a callback to every call; a
// method that ignored it would hang the extension. `shim_tests.rs` calls
// every method this way.
(() => {
  "use strict";
  const CFG = globalThis.__MCEXT_CFG;
  try { delete globalThis.__MCEXT_CFG; } catch (_) { globalThis.__MCEXT_CFG = undefined; }
  if (!CFG) return;
  // Once per context and mode: a page may also be re-injected.
  const GUARD = "__mcextLoaded:" + CFG.mode;
  if (Object.getOwnPropertyDescriptor(globalThis, GUARD)) return;
  Object.defineProperty(globalThis, GUARD, { value: true });

  const ID = CFG.id;
  const BASE = CFG.base;
  const MANIFEST = CFG.manifest || {};
  const MESSAGES = CFG.messages || {};
  const MODE = CFG.mode;
  const isWorker = typeof WorkerGlobalScope !== "undefined" && self instanceof WorkerGlobalScope;
  const isTop = !isWorker && typeof window !== "undefined" && window.top === window;
  const CTX = (typeof crypto !== "undefined" && crypto.randomUUID && crypto.randomUUID()) || String(Math.random()).slice(2) + String(Date.now());
  const KIND = MODE === "content" ? "content" : MODE === "external" ? "external" : isWorker ? "background" : CFG.kind || "page";
  const here = () => (typeof location !== "undefined" ? String(location.href) : "");
  const NO_RECEIVER = "Could not establish connection. Receiving end does not exist.";
  const PORT_CLOSED = "The message port closed before a response was received.";
  const ERR = (m) => new Error(m);

  // ---- transport ----
  let seq = 0;
  const waiting = new Map();
  let send; // (api, args) -> Promise<value>
  const inbox = (e) => dispatch(e);
  if (CFG.transport) {
    // Tests (shim_tests.rs): a transport given in the config.
    send = CFG.transport.call;
    CFG.transport.listen && CFG.transport.listen(inbox);
  } else if (MODE === "page") {
    const nativeFetch = globalThis.fetch.bind(globalThis);
    const headers = { "content-type": "application/json", "x-mcext-token": CFG.token };
    send = async (api, args) => {
      let res;
      try {
        res = await nativeFetch(BASE + "__mcext/api", { method: "POST", headers, body: JSON.stringify({ ctx: CTX, api, args }) });
      } catch (e) {
        throw ERR("Extension context invalidated.");
      }
      const reply = await res.json();
      if (!reply.ok) throw ERR(reply.error || "failed");
      return reply.value;
    };
    (async () => {
      let backoff = 100;
      for (;;) {
        try {
          const res = await nativeFetch(BASE + "__mcext/events?ctx=" + encodeURIComponent(CTX), { headers: { "x-mcext-token": CFG.token }, cache: "no-store" });
          if (res.status === 410) return; // the instance stopped
          const events = await res.json();
          backoff = 100;
          for (const e of events) {
            try { dispatch(e); } catch (err) { console.error(err); }
          }
        } catch (_) {
          await new Promise((r) => setTimeout(r, backoff));
          backoff = Math.min(backoff * 2, 5000);
        }
      }
    })();
    if (!isWorker && typeof addEventListener === "function") {
      addEventListener("pagehide", () => {
        try { nativeFetch(BASE + "__mcext/api", { method: "POST", keepalive: true, headers, body: JSON.stringify({ ctx: CTX, api: "ctx.bye", args: [] }) }); } catch (_) {}
      });
    }
  } else {
    // The program took the binding off the world's global already.
    const bind = CFG.send;
    if (typeof bind !== "function") return;
    const post = (m) => bind(JSON.stringify(m));
    send = (api, args) => new Promise((resolve, reject) => {
      const id = ++seq;
      waiting.set(id, { resolve, reject });
      post({ t: "call", id, ctx: CTX, api, args });
    });
    Object.defineProperty(globalThis, CFG.recv, {
      value: (m) => {
        if (m.t === "reply") {
          const w = waiting.get(m.id);
          waiting.delete(m.id);
          if (w) m.ok ? w.resolve(m.value) : w.reject(ERR(m.error || "failed"));
        } else dispatch(m);
      },
    });
  }
  const call = (api, args) => send(api, args === undefined ? [] : args);

  // ---- errors and callbacks: the one adapter ----
  let lastError;
  let lastErrorChecked = false;
  const withLastError = (message, fn) => {
    lastError = { message };
    lastErrorChecked = false;
    try { fn(); } finally {
      if (!lastErrorChecked) console.warn("Unchecked runtime.lastError: " + message);
      lastError = undefined;
    }
  };
  /// Resolves the promise form with `undefined`, but gives the callback form
  /// `lastError` (runtime.sendMessage without a response).
  const SOFT = Symbol("soft");
  const soft = (message) => ({ [SOFT]: message });
  const finish = (p, cb) => {
    if (!cb) {
      return p.then((v) => (v && typeof v === "object" && SOFT in v ? undefined : v));
    }
    p.then(
      (v) => {
        if (v && typeof v === "object" && SOFT in v) withLastError(v[SOFT], () => cb());
        else cb(v);
      },
      (e) => withLastError((e && e.message) || String(e), () => cb()),
    );
    return undefined;
  };
  const asyncMethod = (impl) => function (...a) {
    const cb = typeof a[a.length - 1] === "function" ? a.pop() : undefined;
    let p;
    try { p = Promise.resolve(impl.apply(this, a)); } catch (e) { p = Promise.reject(e); }
    return finish(p, cb);
  };
  /// A method answered by the app as `api`, its arguments mapped by `prep`.
  const remote = (api, prep) => asyncMethod((...a) => call(api, prep ? prep(a) : a));
  /// Known, not run here: fails the Chrome way.
  const unsupported = (api) => asyncMethod(() => { throw ERR(api + " is not supported in Monocode's browser."); });

  // ---- events ----
  const listeners = new Map();
  const listening = new Set();
  // onInstalled/onStartup reach listeners added up to LIFECYCLE_MS after
  // start too: Chrome starts the worker for them; extensions (Proton Pass)
  // add theirs after async setup.
  const LIFECYCLE_MS = 15000;
  const started = Date.now();
  const lifecycle = new Map();
  const event = (name, opts) => {
    const set = () => {
      let s = listeners.get(name);
      if (!s) listeners.set(name, (s = []));
      return s;
    };
    const ev = {
      addListener(f, filter, extra) {
        if (typeof f !== "function") throw ERR("Listener must be a function");
        if (set().some((l) => l.f === f)) return;
        set().push({ f, filter, extra });
        if (!listening.has(name) && !(opts && opts.local)) {
          listening.add(name);
          call("ctx.listen", [name]).catch(() => {});
        }
        if (lifecycle.has(name) && Date.now() - started < LIFECYCLE_MS) {
          const args = lifecycle.get(name);
          setTimeout(() => { try { f(...args); } catch (e) { console.error(e); } }, 0);
        }
      },
      removeListener(f) {
        const s = set();
        const at = s.findIndex((l) => l.f === f);
        if (at >= 0) s.splice(at, 1);
      },
      hasListener: (f) => set().some((l) => l.f === f),
      hasListeners: () => set().length > 0,
      addRules: asyncMethod(() => { throw ERR("Rules are not supported."); }),
      getRules: asyncMethod(async () => []),
      removeRules: asyncMethod(async () => {}),
    };
    return ev;
  };
  const fire = (name, args, pick) => {
    for (const l of [...(listeners.get(name) || [])]) {
      if (pick && !pick(l)) continue;
      try { l.f(...args); } catch (e) { console.error(e); }
    }
  };

  // ---- messaging ----
  const respond = (rid, has, value) => call("runtime.respond", [rid, has, value]).catch(() => {});
  const deliverMessage = (name, msg, sender, rid) => {
    const fns = [...(listeners.get(name) || [])];
    let answered = false;
    let keep = false;
    const sendResponse = (v) => {
      if (answered) return;
      answered = true;
      respond(rid, true, v === undefined ? null : v);
    };
    for (const { f } of fns) {
      let r;
      try { r = f(msg, sender, sendResponse); } catch (e) { console.error(e); continue; }
      if (r === true) keep = true;
      else if (r && typeof r.then === "function") {
        keep = true;
        r.then((v) => sendResponse(v), (e) => {
          if (!answered) { answered = true; respond(rid, true, { __mcextError: String((e && e.message) || e) }); }
        });
      }
    }
    if (!keep && !answered) respond(rid, false, null);
  };

  const ports = new Map();
  const makePort = (id, name, sender) => {
    let connected = true;
    const onMessage = event("port.message." + id, { local: true });
    const onDisconnect = event("port.disconnect." + id, { local: true });
    const port = {
      name,
      sender,
      onMessage,
      onDisconnect,
      postMessage(m) {
        if (!connected) throw ERR("Attempting to use a disconnected port object");
        call("port.post", [id, m === undefined ? null : m]).catch(() => {});
      },
      disconnect() {
        if (!connected) return;
        connected = false;
        ports.delete(id);
        call("port.disconnect", [id]).catch(() => {});
      },
    };
    port.__close = (error) => {
      if (!connected) return;
      connected = false;
      ports.delete(id);
      const run = () => fire("port.disconnect." + id, [port]);
      if (error) withLastError(error, run); else run();
    };
    ports.set(id, port);
    return port;
  };
  const openPort = (api, args, name) => {
    const id = CTX + ":" + ++seq;
    const port = makePort(id, name || "", undefined);
    call(api, [...args, id, name || ""]).catch((e) => port.__close(e.message || NO_RECEIVER));
    return port;
  };

  function dispatch(e) {
    switch (e.t) {
      case "message":
        deliverMessage(e.external ? "runtime.onMessageExternal" : "runtime.onMessage", e.msg, e.sender, e.rid);
        break;
      case "connect": {
        const port = makePort(e.port, e.name, e.sender);
        fire(e.external ? "runtime.onConnectExternal" : "runtime.onConnect", [port]);
        break;
      }
      case "port.message": {
        const port = ports.get(e.port);
        if (port) fire("port.message." + e.port, [e.msg, port]);
        break;
      }
      case "port.disconnect": {
        const port = ports.get(e.port);
        if (port) port.__close(e.error);
        break;
      }
      case "event":
        fire(e.name, e.args || [], e.filter ? (l) => eventFilter(e, l) : undefined);
        break;
      case "close":
        // The app closes this context (a popup on blur, a stopped instance).
        try { if (!isWorker) window.close(); } catch (_) {}
        break;
    }
  }

  // webRequest/webNavigation listeners' filters: urls and types.
  const patternRegex = (p) => {
    if (p === "<all_urls>") return /^(https?|wss?|file):/;
    const m = /^(\*|https?|wss?|file|ftp|urn):\/\/([^/]*)(\/.*)?$/.exec(p);
    if (!m) return /^$/;
    const esc = (s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
    const scheme = m[1] === "*" ? "https?" : esc(m[1]);
    let host = m[2];
    let port = "(:\\d+)?";
    const hp = /^(.*):(\d+|\*)$/.exec(host);
    if (hp && !host.endsWith("]")) { host = hp[1]; if (hp[2] !== "*") port = ":" + hp[2]; }
    const h = host === "*" ? "[^/]*" : host.startsWith("*.") ? "([^/]*\\.)?" + esc(host.slice(2)) : esc(host);
    const path = (m[3] || "/").split("*").map(esc).join(".*");
    return new RegExp("^" + scheme + "://" + h + port + path + "$", "i");
  };
  function eventFilter(e, l) {
    const f = l.filter;
    const d = (e.args || [])[0] || {};
    if (!f) return true;
    if (Array.isArray(f.urls) && f.urls.length && !f.urls.some((p) => patternRegex(p).test(d.url || ""))) return false;
    if (Array.isArray(f.types) && f.types.length && !f.types.includes(d.type)) return false;
    if (f.tabId !== undefined && f.tabId !== d.tabId) return false;
    if (Array.isArray(f.url) && f.url.length && !f.url.some((u) => urlFilter(u, d.url || ""))) return false;
    return true;
  }
  // webNavigation's UrlFilter (the common keys).
  function urlFilter(u, url) {
    let p;
    try { p = new URL(url); } catch (_) { return false; }
    const host = p.hostname;
    if (u.hostEquals && host !== u.hostEquals) return false;
    if (u.hostSuffix && !host.endsWith(u.hostSuffix)) return false;
    if (u.hostContains && !host.includes(u.hostContains)) return false;
    if (u.urlContains && !url.includes(u.urlContains)) return false;
    if (u.urlPrefix && !url.startsWith(u.urlPrefix)) return false;
    if (u.urlMatches && !new RegExp(u.urlMatches).test(url)) return false;
    if (u.schemes && !u.schemes.includes(p.protocol.slice(0, -1))) return false;
    if (u.pathPrefix && !p.pathname.startsWith(u.pathPrefix)) return false;
    return true;
  }

  // ---- runtime ----
  const getURL = (p) => BASE + String(p == null ? "" : p).replace(/^\//, "");
  const sendMessageArgs = (a) => {
    // (extensionId?, message, options?)
    let target = null;
    let msg;
    let options;
    if (a.length >= 3) [target, msg, options] = a;
    else if (a.length === 2) {
      const looksLikeOptions = a[1] && typeof a[1] === "object" && Object.keys(a[1]).every((k) => k === "includeTlsChannelId");
      if ((typeof a[0] === "string" || a[0] == null) && !(looksLikeOptions && Object.keys(a[1]).length)) [target, msg] = a;
      else [msg, options] = a;
    } else msg = a[0];
    return [target == null ? null : target, msg === undefined ? null : msg, options || {}];
  };
  const messageResult = (v) => {
    if (v && v.__mcextNone) return soft(PORT_CLOSED);
    if (v && v.__mcextError !== undefined) throw ERR(v.__mcextError);
    return v === null ? undefined : v;
  };
  const runtime = {
    id: ID,
    getURL,
    getManifest: () => JSON.parse(JSON.stringify(MANIFEST)),
    get lastError() { lastErrorChecked = true; return lastError; },
    sendMessage: asyncMethod((...a) => call("runtime.sendMessage", sendMessageArgs(a)).then(messageResult)),
    connect(...a) {
      let target = null;
      if (typeof a[0] === "string") target = a.shift();
      const info = a[0] || {};
      return openPort("runtime.connect", [target], info.name);
    },
    connectNative(name) {
      const port = makePort(CTX + ":" + ++seq, String(name), undefined);
      setTimeout(() => port.__close("Specified native messaging host not found."), 0);
      return port;
    },
    sendNativeMessage: asyncMethod(() => { throw ERR("Specified native messaging host not found."); }),
    onMessage: event("runtime.onMessage"),
    onMessageExternal: event("runtime.onMessageExternal"),
    onConnect: event("runtime.onConnect"),
    onConnectExternal: event("runtime.onConnectExternal"),
    onConnectNative: event("runtime.onConnectNative", { local: true }),
    onInstalled: event("runtime.onInstalled", { local: true }),
    onStartup: event("runtime.onStartup", { local: true }),
    onSuspend: event("runtime.onSuspend", { local: true }),
    onSuspendCanceled: event("runtime.onSuspendCanceled", { local: true }),
    onUpdateAvailable: event("runtime.onUpdateAvailable"),
    onRestartRequired: event("runtime.onRestartRequired", { local: true }),
    onUserScriptMessage: event("runtime.onUserScriptMessage", { local: true }),
    getPlatformInfo: remote("runtime.getPlatformInfo"),
    getContexts: remote("runtime.getContexts", (a) => [a[0] || {}]),
    getBackgroundPage: asyncMethod(() => { throw ERR("You do not have a background page."); }),
    openOptionsPage: remote("runtime.openOptionsPage"),
    setUninstallURL: remote("runtime.setUninstallURL"),
    requestUpdateCheck: remote("runtime.requestUpdateCheck"),
    restart: unsupported("runtime.restart"),
    restartAfterDelay: unsupported("runtime.restartAfterDelay"),
    reload() { call("runtime.reload", []).catch(() => {}); },
    getPackageDirectoryEntry: unsupported("runtime.getPackageDirectoryEntry"),
    ContextType: { BACKGROUND: "BACKGROUND", POPUP: "POPUP", TAB: "TAB", OFFSCREEN_DOCUMENT: "OFFSCREEN_DOCUMENT", SIDE_PANEL: "SIDE_PANEL", DEVELOPER_TOOLS: "DEVELOPER_TOOLS" },
    OnInstalledReason: { INSTALL: "install", UPDATE: "update", CHROME_UPDATE: "chrome_update", SHARED_MODULE_UPDATE: "shared_module_update" },
    OnRestartRequiredReason: { APP_UPDATE: "app_update", OS_UPDATE: "os_update", PERIODIC: "periodic" },
    PlatformOs: { MAC: "mac", WIN: "win", ANDROID: "android", CROS: "cros", LINUX: "linux", OPENBSD: "openbsd", FUCHSIA: "fuchsia" },
    PlatformArch: { ARM: "arm", ARM64: "arm64", X86_32: "x86-32", X86_64: "x86-64", MIPS: "mips", MIPS64: "mips64" },
  };

  if (MODE === "external") {
    // The relay: the page's main world (external.js) asks over DOM events,
    // the app answers as for any context; sender.origin is the page's.
    const channel = CFG.channel;
    const reply = (detail) => window.dispatchEvent(new CustomEvent(channel + ":r", { detail: JSON.stringify(detail) }));
    const pagePorts = new Map();
    window.addEventListener(channel, (ev) => {
      let m;
      try { m = JSON.parse(ev.detail); } catch (_) { return; }
      if (!m || typeof m !== "object") return;
      if (m.t === "send") {
        call("runtime.sendMessage", [m.target || null, m.msg === undefined ? null : m.msg, {}])
          .then(messageResult)
          .then((v) => reply({ t: "reply", id: m.id, ok: true, value: v && SOFT in v ? undefined : v, none: !!(v && SOFT in v) }),
            (e) => reply({ t: "reply", id: m.id, ok: false, error: e.message }));
      } else if (m.t === "connect") {
        const port = runtime.connect(m.target || ID, { name: m.name || "" });
        pagePorts.set(m.port, port);
        port.onMessage.addListener((msg) => reply({ t: "port.message", port: m.port, msg }));
        port.onDisconnect.addListener(() => { pagePorts.delete(m.port); reply({ t: "port.disconnect", port: m.port, error: lastError && lastError.message }); });
      } else if (m.t === "port.post") {
        const port = pagePorts.get(m.port);
        if (port) { try { port.postMessage(m.msg); } catch (_) {} }
      } else if (m.t === "port.disconnect") {
        const port = pagePorts.get(m.port);
        pagePorts.delete(m.port);
        if (port) port.disconnect();
      }
    });
    call("ctx.hello", [{ kind: KIND, url: here(), top: isTop }]).catch(() => {});
    return;
  }

  // ---- storage ----
  const area = (name) => {
    const a = {
      get: remote("storage.get", (x) => [name, x[0] === undefined ? null : x[0]]),
      set: remote("storage.set", (x) => [name, x[0] || {}]),
      remove: remote("storage.remove", (x) => [name, x[0]]),
      clear: remote("storage.clear", () => [name]),
      getBytesInUse: remote("storage.getBytesInUse", (x) => [name, x[0] === undefined ? null : x[0]]),
      getKeys: remote("storage.getKeys", () => [name]),
      setAccessLevel: remote("storage.setAccessLevel", (x) => [name, x[0] || {}]),
      onChanged: event("storage." + name + ".onChanged"),
    };
    if (name === "sync") Object.assign(a, { QUOTA_BYTES: 102400, QUOTA_BYTES_PER_ITEM: 8192, MAX_ITEMS: 512, MAX_WRITE_OPERATIONS_PER_HOUR: 1800, MAX_WRITE_OPERATIONS_PER_MINUTE: 120 });
    else if (name === "managed") { delete a.set; delete a.remove; delete a.clear; delete a.setAccessLevel; }
    else a.QUOTA_BYTES = 10485760;
    return a;
  };
  const storage = {
    local: area("local"),
    sync: area("sync"),
    session: area("session"),
    managed: area("managed"),
    onChanged: event("storage.onChanged"),
    AccessLevel: { TRUSTED_CONTEXTS: "TRUSTED_CONTEXTS", TRUSTED_AND_UNTRUSTED_CONTEXTS: "TRUSTED_AND_UNTRUSTED_CONTEXTS" },
  };

  // ---- i18n ----
  const getMessage = (name, subs) => {
    const key = String(name).toLowerCase();
    if (key === "@@extension_id") return ID;
    if (key === "@@ui_locale") return (CFG.locale || "en").replace("-", "_");
    if (key === "@@bidi_dir") return "ltr";
    if (key === "@@bidi_reversed_dir") return "rtl";
    if (key === "@@bidi_start_edge") return "left";
    if (key === "@@bidi_end_edge") return "right";
    let text = MESSAGES[key];
    if (text === undefined) return "";
    const list = subs === undefined ? [] : Array.isArray(subs) ? subs : [subs];
    return text.replace(/\$(\d)/g, (_, n) => (list[n - 1] === undefined ? "" : String(list[n - 1]))).replace(/\$\$/g, "$");
  };
  const i18n = {
    getMessage,
    getUILanguage: () => CFG.locale || "en-US",
    getAcceptLanguages: asyncMethod(async () => [CFG.locale || "en-US"]),
    detectLanguage: asyncMethod(async () => ({ isReliable: false, languages: [] })),
  };

  const extension = {
    getURL,
    inIncognitoContext: false,
    isAllowedIncognitoAccess: asyncMethod(async () => false),
    isAllowedFileSchemeAccess: asyncMethod(async () => false),
    setUpdateUrlData: () => {},
    getViews: () => [],
    getBackgroundPage: () => null,
    ViewType: { TAB: "tab", POPUP: "popup" },
  };

  if (MODE === "content") {
    // Content scripts get part of the API, as in Chrome.
    const chrome = {
      runtime: {
        id: ID, getURL, getManifest: runtime.getManifest,
        get lastError() { return runtime.lastError; },
        sendMessage: runtime.sendMessage, connect: runtime.connect,
        onMessage: runtime.onMessage, onConnect: runtime.onConnect,
        ContextType: runtime.ContextType,
      },
      storage, i18n,
      extension: { getURL, inIncognitoContext: false },
    };
    install(chrome);
    call("ctx.hello", [{ kind: KIND, url: here(), top: isTop }]).catch(() => {});
    return;
  }

  // ---- tabs and windows ----
  const fnSource = (f) => (typeof f === "function" ? f.toString() : String(f));
  const tabs = {
    TAB_ID_NONE: -1,
    TAB_INDEX_NONE: -1,
    MAX_CAPTURE_VISIBLE_TAB_CALLS_PER_SECOND: 2,
    query: remote("tabs.query", (a) => [a[0] || {}]),
    get: remote("tabs.get"),
    getCurrent: remote("tabs.getCurrent"),
    create: remote("tabs.create", (a) => [a[0] || {}]),
    duplicate: remote("tabs.duplicate"),
    update: remote("tabs.update", (a) => (typeof a[0] === "number" ? [a[0], a[1] || {}] : [null, a[0] || {}])),
    remove: remote("tabs.remove", (a) => [Array.isArray(a[0]) ? a[0] : [a[0]]]),
    reload: remote("tabs.reload", (a) => (typeof a[0] === "number" ? [a[0], a[1] || {}] : [null, a[0] || {}])),
    goBack: remote("tabs.goBack", (a) => [typeof a[0] === "number" ? a[0] : null]),
    goForward: remote("tabs.goForward", (a) => [typeof a[0] === "number" ? a[0] : null]),
    discard: remote("tabs.discard", (a) => [typeof a[0] === "number" ? a[0] : null]),
    highlight: remote("tabs.highlight", (a) => [a[0] || {}]),
    move: unsupported("tabs.move"),
    group: unsupported("tabs.group"),
    ungroup: unsupported("tabs.ungroup"),
    detectLanguage: asyncMethod(async () => "und"),
    captureVisibleTab: unsupported("tabs.captureVisibleTab"),
    getZoom: remote("tabs.getZoom", (a) => [typeof a[0] === "number" ? a[0] : null]),
    setZoom: unsupported("tabs.setZoom"),
    getZoomSettings: asyncMethod(async () => ({ mode: "automatic", scope: "per-origin", defaultZoomFactor: 1 })),
    setZoomSettings: unsupported("tabs.setZoomSettings"),
    sendMessage: asyncMethod((tabId, msg, options) => call("tabs.sendMessage", [tabId, msg === undefined ? null : msg, options || {}]).then(messageResult)),
    connect(tabId, info) {
      info = info || {};
      return openPort("tabs.connect", [tabId, info.frameId === undefined ? null : info.frameId, info.documentId || null], info.name);
    },
    onUpdated: event("tabs.onUpdated"),
    onRemoved: event("tabs.onRemoved"),
    onActivated: event("tabs.onActivated"),
    onCreated: event("tabs.onCreated"),
    onReplaced: event("tabs.onReplaced"),
    onHighlighted: event("tabs.onHighlighted"),
    onMoved: event("tabs.onMoved"),
    onAttached: event("tabs.onAttached"),
    onDetached: event("tabs.onDetached"),
    onZoomChange: event("tabs.onZoomChange"),
  };
  const windows = {
    WINDOW_ID_NONE: -1,
    WINDOW_ID_CURRENT: -2,
    get: remote("windows.get", (a) => [a[0], a[1] || {}]),
    getCurrent: remote("windows.getCurrent", (a) => [a[0] || {}]),
    getLastFocused: remote("windows.getLastFocused", (a) => [a[0] || {}]),
    getAll: remote("windows.getAll", (a) => [a[0] || {}]),
    create: remote("windows.create", (a) => [a[0] || {}]),
    update: remote("windows.update", (a) => [a[0], a[1] || {}]),
    remove: remote("windows.remove"),
    onFocusChanged: event("windows.onFocusChanged"),
    onRemoved: event("windows.onRemoved"),
    onCreated: event("windows.onCreated"),
    onBoundsChanged: event("windows.onBoundsChanged"),
  };

  // ---- action ----
  const imageData = async (details) => {
    // ImageData is not JSON: RGBA bytes as base64, with the size.
    const one = (img) => {
      if (!img || !img.data) return null;
      let s = "";
      const u8 = new Uint8Array(img.data.buffer || img.data);
      for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
      return { width: img.width, height: img.height, rgba: btoa(s) };
    };
    const out = { tabId: details.tabId === undefined ? null : details.tabId };
    if (details.path !== undefined) out.path = details.path;
    if (details.imageData !== undefined) {
      const d = details.imageData;
      out.imageData = d && d.data ? { "16": one(d) } : Object.fromEntries(Object.entries(d || {}).map(([k, v]) => [k, one(v)]));
    }
    return out;
  };
  const action = {
    setIcon: asyncMethod(async (details) => call("action.setIcon", [await imageData(details || {})])),
    setBadgeText: remote("action.set", (a) => ["badgeText", a[0] || {}]),
    getBadgeText: remote("action.get", (a) => ["badgeText", a[0] || {}]),
    setBadgeBackgroundColor: remote("action.set", (a) => ["badgeBackgroundColor", a[0] || {}]),
    getBadgeBackgroundColor: remote("action.get", (a) => ["badgeBackgroundColor", a[0] || {}]),
    setBadgeTextColor: remote("action.set", (a) => ["badgeTextColor", a[0] || {}]),
    getBadgeTextColor: remote("action.get", (a) => ["badgeTextColor", a[0] || {}]),
    setTitle: remote("action.set", (a) => ["title", a[0] || {}]),
    getTitle: remote("action.get", (a) => ["title", a[0] || {}]),
    setPopup: remote("action.set", (a) => ["popup", a[0] || {}]),
    getPopup: remote("action.get", (a) => ["popup", a[0] || {}]),
    enable: remote("action.set", (a) => ["enabled", { tabId: typeof a[0] === "number" ? a[0] : null, value: true }]),
    disable: remote("action.set", (a) => ["enabled", { tabId: typeof a[0] === "number" ? a[0] : null, value: false }]),
    isEnabled: remote("action.get", (a) => ["enabled", { tabId: typeof a[0] === "number" ? a[0] : null }]),
    getUserSettings: asyncMethod(async () => ({ isOnToolbar: true })),
    openPopup: remote("action.openPopup", (a) => [a[0] || {}]),
    onClicked: event("action.onClicked"),
    onUserSettingsChanged: event("action.onUserSettingsChanged", { local: true }),
  };

  // ---- alarms (the app keeps them, so every context sees the same) ----
  const alarms = {
    create: remote("alarms.create", (a) => (typeof a[0] === "string" || a[0] == null && a.length > 1 ? [a[0] || "", a[1] || {}] : ["", a[0] || {}])),
    get: remote("alarms.get", (a) => [a[0] || ""]),
    getAll: remote("alarms.getAll"),
    clear: remote("alarms.clear", (a) => [a[0] || ""]),
    clearAll: remote("alarms.clearAll"),
    onAlarm: event("alarms.onAlarm"),
  };

  const stubEvent = (name) => event(name, { local: true });
  const setting = (name) => ({
    get: remote("privacy.get", (a) => [name, a[0] || {}]),
    set: remote("privacy.set", (a) => [name, a[0] || {}]),
    clear: remote("privacy.clear", (a) => [name, a[0] || {}]),
    onChange: event("privacy.onChange." + name),
  });

  const chrome = {
    runtime,
    storage,
    alarms,
    tabs,
    windows,
    action,
    browserAction: undefined,
    i18n,
    extension,
    scripting: {
      executeScript: remote("scripting.executeScript", (a) => [{ ...(a[0] || {}), func: a[0] && a[0].func ? fnSource(a[0].func) : undefined, function: undefined }]),
      insertCSS: remote("scripting.insertCSS", (a) => [a[0] || {}]),
      removeCSS: remote("scripting.removeCSS", (a) => [a[0] || {}]),
      registerContentScripts: remote("scripting.registerContentScripts", (a) => [a[0] || []]),
      getRegisteredContentScripts: remote("scripting.getRegisteredContentScripts", (a) => [a[0] || {}]),
      unregisterContentScripts: remote("scripting.unregisterContentScripts", (a) => [a[0] || {}]),
      updateContentScripts: remote("scripting.updateContentScripts", (a) => [a[0] || []]),
      ExecutionWorld: { ISOLATED: "ISOLATED", MAIN: "MAIN" },
      StyleOrigin: { AUTHOR: "AUTHOR", USER: "USER" },
    },
    webNavigation: {
      getFrame: remote("webNavigation.getFrame", (a) => [a[0] || {}]),
      getAllFrames: remote("webNavigation.getAllFrames", (a) => [a[0] || {}]),
      onBeforeNavigate: event("webNavigation.onBeforeNavigate"),
      onCommitted: event("webNavigation.onCommitted"),
      onDOMContentLoaded: event("webNavigation.onDOMContentLoaded"),
      onCompleted: event("webNavigation.onCompleted"),
      onErrorOccurred: event("webNavigation.onErrorOccurred"),
      onHistoryStateUpdated: event("webNavigation.onHistoryStateUpdated"),
      onReferenceFragmentUpdated: event("webNavigation.onReferenceFragmentUpdated"),
      onCreatedNavigationTarget: event("webNavigation.onCreatedNavigationTarget"),
      onTabReplaced: event("webNavigation.onTabReplaced"),
    },
    webRequest: {
      MAX_HANDLER_BEHAVIOR_CHANGED_CALLS_PER_10_MINUTES: 20,
      handlerBehaviorChanged: asyncMethod(async () => {}),
      onBeforeRequest: event("webRequest.onBeforeRequest"),
      onBeforeSendHeaders: event("webRequest.onBeforeSendHeaders"),
      onSendHeaders: event("webRequest.onSendHeaders"),
      onHeadersReceived: event("webRequest.onHeadersReceived"),
      onAuthRequired: event("webRequest.onAuthRequired"),
      onResponseStarted: event("webRequest.onResponseStarted"),
      onBeforeRedirect: event("webRequest.onBeforeRedirect"),
      onCompleted: event("webRequest.onCompleted"),
      onErrorOccurred: event("webRequest.onErrorOccurred"),
      onActionIgnored: stubEvent("webRequest.onActionIgnored"),
    },
    offscreen: {
      createDocument: remote("offscreen.createDocument", (a) => [a[0] || {}]),
      closeDocument: remote("offscreen.closeDocument"),
      hasDocument: remote("offscreen.hasDocument"),
      Reason: Object.fromEntries(["TESTING", "AUDIO_PLAYBACK", "IFRAME_SCRIPTING", "DOM_SCRAPING", "BLOBS", "DOM_PARSER", "USER_MEDIA", "DISPLAY_MEDIA", "WEB_RTC", "CLIPBOARD", "LOCAL_STORAGE", "WORKERS", "BATTERY_STATUS", "MATCH_MEDIA", "GEOLOCATION"].map((r) => [r, r])),
    },
    permissions: {
      contains: remote("permissions.contains", (a) => [a[0] || {}]),
      getAll: remote("permissions.getAll"),
      request: remote("permissions.request", (a) => [a[0] || {}]),
      remove: remote("permissions.remove", (a) => [a[0] || {}]),
      addHostAccessRequest: asyncMethod(async () => {}),
      removeHostAccessRequest: asyncMethod(async () => {}),
      onAdded: event("permissions.onAdded"),
      onRemoved: event("permissions.onRemoved"),
    },
    privacy: {
      services: Object.fromEntries(["passwordSavingEnabled", "autofillEnabled", "autofillAddressEnabled", "autofillCreditCardEnabled", "alternateErrorPagesEnabled", "safeBrowsingEnabled", "searchSuggestEnabled", "spellingServiceEnabled", "translationServiceEnabled"].map((n) => [n, setting("services." + n)])),
      network: Object.fromEntries(["networkPredictionEnabled", "webRTCIPHandlingPolicy"].map((n) => [n, setting("network." + n)])),
      websites: Object.fromEntries(["thirdPartyCookiesAllowed", "hyperlinkAuditingEnabled", "referrersEnabled", "doNotTrackEnabled", "protectedContentEnabled"].map((n) => [n, setting("websites." + n)])),
    },
    commands: {
      getAll: remote("commands.getAll"),
      onCommand: event("commands.onCommand"),
    },
    management: {
      getSelf: remote("management.getSelf"),
      uninstallSelf: unsupported("management.uninstallSelf"),
      getPermissionWarningsByManifest: unsupported("management.getPermissionWarningsByManifest"),
      onEnabled: stubEvent("management.onEnabled"),
      onDisabled: stubEvent("management.onDisabled"),
      onInstalled: stubEvent("management.onInstalled"),
      onUninstalled: stubEvent("management.onUninstalled"),
    },
    contextMenus: {
      ACTION_MENU_TOP_LEVEL_LIMIT: 6,
      create(props, cb) {
        const id = (props && props.id) || "mcext-" + ++seq;
        if (cb) setTimeout(cb, 0);
        return id;
      },
      update: asyncMethod(async () => {}),
      remove: asyncMethod(async () => {}),
      removeAll: asyncMethod(async () => {}),
      onClicked: stubEvent("contextMenus.onClicked"),
    },
    notifications: {
      create: unsupported("notifications.create"),
      update: asyncMethod(async () => false),
      clear: asyncMethod(async () => false),
      getAll: asyncMethod(async () => ({})),
      getPermissionLevel: asyncMethod(async () => "denied"),
      onClicked: stubEvent("notifications.onClicked"),
      onClosed: stubEvent("notifications.onClosed"),
      onButtonClicked: stubEvent("notifications.onButtonClicked"),
      onShowSettings: stubEvent("notifications.onShowSettings"),
      onPermissionLevelChanged: stubEvent("notifications.onPermissionLevelChanged"),
    },
    idle: {
      queryState: asyncMethod(async () => "active"),
      setDetectionInterval: () => {},
      getAutoLockDelay: asyncMethod(async () => 0),
      onStateChanged: stubEvent("idle.onStateChanged"),
    },
    identity: {
      getAuthToken: unsupported("identity.getAuthToken"),
      launchWebAuthFlow: unsupported("identity.launchWebAuthFlow"),
      getRedirectURL: (p) => "https://" + ID + ".chromiumapp.org/" + (p || ""),
    },
  };
  delete chrome.browserAction;
  if (!(MANIFEST.permissions || []).includes("identity")) delete chrome.identity;

  function install(chromeObject) {
    // Ours replaces Chromium's own (`chrome.loadTimes`, …) in this context.
    try {
      Object.defineProperty(globalThis, "chrome", { value: chromeObject, writable: true, configurable: true, enumerable: false });
    } catch (_) {
      globalThis.chrome = chromeObject;
    }
  }
  install(chrome);
  if (CFG.expose) Object.defineProperty(globalThis, "__mcextChrome", { value: chrome });

  call("ctx.hello", [{ kind: KIND, url: here(), top: isTop }]).then((info) => {
    if (!info) return;
    // Popups close on Esc, as Chrome's, unless the page handled the key.
    if (info.kind === "popup" && typeof addEventListener === "function") {
      addEventListener("keydown", (e) => {
        if (e.key === "Escape" && !e.defaultPrevented) call("popup.close", []).catch(() => {});
      });
    }
    if (info.installed) {
      const details = { reason: info.installed };
      if (info.previousVersion) details.previousVersion = info.previousVersion;
      lifecycle.set("runtime.onInstalled", [details]);
      fire("runtime.onInstalled", [details]);
    }
    if (info.startup) {
      lifecycle.set("runtime.onStartup", []);
      fire("runtime.onStartup", []);
    }
  }).catch((e) => console.debug("[mcext] hello failed", e));
})();
