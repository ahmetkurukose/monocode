// `chrome.runtime.sendMessage/connect` for a web page an extension's
// `externally_connectable` names (main world, before the page's scripts).
// Nothing here can reach the app: it asks the extension's relay in the
// isolated world (shim.js, mode "external") over DOM events, on a channel
// named by `__MCEXT_EXT` (see scripts.rs). Several extensions may share a
// page: each adds its id.
(() => {
  "use strict";
  const CFG = globalThis.__MCEXT_EXT;
  try { delete globalThis.__MCEXT_EXT; } catch (_) { globalThis.__MCEXT_EXT = undefined; }
  if (!CFG) return;
  const registry = (globalThis[CFG.registry] = globalThis[CFG.registry] || new Map());
  registry.set(CFG.id, CFG.channel);
  if (globalThis[CFG.registry + "ready"]) return;
  Object.defineProperty(globalThis, CFG.registry + "ready", { value: true });
  Object.defineProperty(globalThis, CFG.registry, { value: registry, enumerable: false });

  let seq = 0;
  const waiting = new Map();
  const ports = new Map();
  let lastError;
  const NO_RECEIVER = "Could not establish connection. Receiving end does not exist.";
  const channelOf = (id) => registry.get(id);
  const ask = (channel, m) => window.dispatchEvent(new CustomEvent(channel, { detail: JSON.stringify(m) }));
  const listened = new Set();
  const listen = (channel) => {
    if (listened.has(channel)) return;
    listened.add(channel);
    window.addEventListener(channel + ":r", (ev) => {
      let m;
      try { m = JSON.parse(ev.detail); } catch (_) { return; }
      if (m.t === "reply") {
        const w = waiting.get(m.id);
        waiting.delete(m.id);
        if (w) w(m);
      } else if (m.t === "port.message") {
        const p = ports.get(m.port);
        if (p) p.fire("message", [m.msg, p.port]);
      } else if (m.t === "port.disconnect") {
        const p = ports.get(m.port);
        ports.delete(m.port);
        if (p) {
          lastError = m.error ? { message: m.error } : undefined;
          try { p.fire("disconnect", [p.port]); } finally { lastError = undefined; }
        }
      }
    });
  };
  const event = () => {
    const fns = [];
    return {
      fns,
      api: {
        addListener: (f) => { if (!fns.includes(f)) fns.push(f); },
        removeListener: (f) => { const i = fns.indexOf(f); if (i >= 0) fns.splice(i, 1); },
        hasListener: (f) => fns.includes(f),
        hasListeners: () => fns.length > 0,
      },
    };
  };
  const sendMessage = (...a) => {
    const cb = typeof a[a.length - 1] === "function" ? a.pop() : undefined;
    const [target, msg] = a;
    const channel = channelOf(target);
    const p = new Promise((resolve, reject) => {
      if (!channel) return reject(new Error(NO_RECEIVER));
      listen(channel);
      const id = ++seq;
      waiting.set(id, (m) => (m.ok ? resolve(m) : reject(new Error(m.error))));
      ask(channel, { t: "send", id, target, msg });
    });
    if (!cb) return p.then((m) => m.value);
    p.then(
      (m) => {
        if (m.none) {
          lastError = { message: "The message port closed before a response was received." };
          try { cb(); } finally { lastError = undefined; }
        } else cb(m.value);
      },
      (e) => {
        lastError = { message: e.message };
        try { cb(); } finally { lastError = undefined; }
      },
    );
  };
  const connect = (target, info) => {
    const channel = channelOf(target);
    const id = "p" + ++seq + ":" + Math.random().toString(36).slice(2);
    const onMessage = event();
    const onDisconnect = event();
    let open = !!channel;
    const port = {
      name: (info && info.name) || "",
      onMessage: onMessage.api,
      onDisconnect: onDisconnect.api,
      postMessage(m) {
        if (!open) throw new Error("Attempting to use a disconnected port object");
        ask(channel, { t: "port.post", port: id, msg: m });
      },
      disconnect() {
        if (!open) return;
        open = false;
        ports.delete(id);
        ask(channel, { t: "port.disconnect", port: id });
      },
    };
    const entry = {
      port,
      fire(kind, args) {
        if (kind === "disconnect") open = false;
        for (const f of [...(kind === "message" ? onMessage.fns : onDisconnect.fns)]) {
          try { f(...args); } catch (e) { console.error(e); }
        }
      },
    };
    if (!channel) {
      setTimeout(() => {
        lastError = { message: NO_RECEIVER };
        try { entry.fire("disconnect", [port]); } finally { lastError = undefined; }
      }, 0);
      return port;
    }
    listen(channel);
    ports.set(id, entry);
    ask(channel, { t: "connect", port: id, target, name: port.name });
    return port;
  };
  const runtime = {
    sendMessage,
    connect,
    get lastError() { return lastError; },
  };
  const existing = globalThis.chrome;
  if (existing && typeof existing === "object") {
    try { existing.runtime = runtime; } catch (_) {}
    if (existing.runtime !== runtime) {
      try { Object.defineProperty(existing, "runtime", { value: runtime, configurable: true }); } catch (_) {}
    }
  } else {
    Object.defineProperty(globalThis, "chrome", { value: { runtime }, writable: true, configurable: true });
  }
})();
