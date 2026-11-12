// The Postman scripting surface (`pm`, `postman`, `tests`, `console`, `expect`)
// that pre-request and test scripts run against. State goes in as JSON
// (`__in`) and comes out as JSON (`__out`); nothing here touches the host.
(function () {
  "use strict";
  const g = globalThis;
  const input = JSON.parse(g.__in);
  delete g.__in;

  const str = (v) =>
    v === undefined
      ? "undefined"
      : typeof v === "string"
        ? v
        : typeof v === "object" && v !== null
          ? JSON.stringify(v)
          : String(v);
  const show = (v) => {
    try {
      if (v === undefined) return "undefined";
      if (typeof v === "function") return "[Function]";
      return JSON.stringify(v);
    } catch (e) {
      return String(v);
    }
  };

  // ---- console ----
  const logs = [];
  const line = (args) =>
    Array.prototype.map
      .call(args, (x) => (typeof x === "string" ? x : typeof x === "function" ? String(x) : show(x)))
      .join(" ");
  g.console = {
    log: (...a) => logs.push(line(a)),
    info: (...a) => logs.push(line(a)),
    debug: (...a) => logs.push(line(a)),
    warn: (...a) => logs.push("warning: " + line(a)),
    error: (...a) => logs.push("error: " + line(a)),
  };

  // ---- variables ----
  class Vars {
    constructor(pairs) {
      this.m = new Map(pairs);
    }
    get(k) {
      return this.m.get(String(k));
    }
    set(k, v) {
      this.m.set(String(k), str(v));
    }
    unset(k) {
      this.m.delete(String(k));
    }
    has(k) {
      return this.m.has(String(k));
    }
    clear() {
      this.m.clear();
    }
    toObject() {
      return Object.fromEntries(this.m);
    }
    replaceIn(t) {
      return resolve(String(t), 0);
    }
    entries() {
      return Array.from(this.m.entries());
    }
  }
  const environment = new Vars(input.environment);
  const collection = new Vars(input.collection);
  const globals = new Vars(input.globals);
  const local = new Vars(input.local);
  environment.name = input.environmentName;
  const lookup = (k) =>
    local.has(k)
      ? local.get(k)
      : environment.has(k)
        ? environment.get(k)
        : collection.has(k)
          ? collection.get(k)
          : globals.get(k);
  function resolve(text, depth) {
    return text.replace(/\{\{\s*([^{}]+?)\s*\}\}/g, (m, k) => {
      const v = lookup(k);
      return v === undefined || depth > 8 ? m : resolve(v, depth + 1);
    });
  }
  const variables = {
    get: (k) => lookup(String(k)),
    set: (k, v) => local.set(k, v),
    has: (k) => lookup(String(k)) !== undefined,
    unset: (k) => local.unset(k),
    clear: () => local.clear(),
    replaceIn: (t) => resolve(String(t), 0),
    toObject: () =>
      Object.assign({}, globals.toObject(), collection.toObject(), environment.toObject(), local.toObject()),
  };

  // ---- headers, url, request ----
  class Headers {
    constructor(list) {
      this.list = list.map(([key, value]) => ({ key, value }));
    }
    add(h) {
      this.list.push({ key: String(h.key), value: str(h.value) });
    }
    upsert(h) {
      const lk = String(h.key).toLowerCase();
      const i = this.list.findIndex((x) => x.key.toLowerCase() === lk);
      if (i >= 0) this.list[i] = { key: String(h.key), value: str(h.value) };
      else this.add(h);
    }
    remove(k) {
      const lk = String(k).toLowerCase();
      this.list = this.list.filter((x) => x.key.toLowerCase() !== lk);
    }
    get(k) {
      const lk = String(k).toLowerCase();
      const f = this.list.find((x) => x.key.toLowerCase() === lk);
      return f ? f.value : undefined;
    }
    has(k) {
      return this.get(k) !== undefined;
    }
    each(fn) {
      this.list.forEach(fn);
    }
    all() {
      return this.list.slice();
    }
    count() {
      return this.list.length;
    }
    toObject() {
      const o = {};
      this.list.forEach((h) => (o[h.key] = h.value));
      return o;
    }
  }
  class Url {
    constructor(raw) {
      this.raw = raw;
    }
    toString() {
      return this.raw;
    }
    getHost() {
      return this.raw.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").split(/[/?#]/)[0].replace(/:\d+$/, "");
    }
    getPath() {
      const m = this.raw.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").match(/^[^/?#]*(\/[^?#]*)?/);
      return (m && m[1]) || "/";
    }
    getQueryString() {
      const i = this.raw.indexOf("?");
      return i < 0 ? "" : this.raw.slice(i + 1).split("#")[0];
    }
  }
  const req = input.request;
  const pmRequest = {
    name: input.requestName,
    method: req.method,
    headers: new Headers(req.headers),
    body: req.body === null ? { mode: undefined } : { mode: "raw", raw: req.body },
  };
  Object.defineProperty(pmRequest.body, "update", {
    enumerable: false,
    value: (o) => {
      if (typeof o === "string") Object.assign(pmRequest.body, { mode: "raw", raw: o });
      else if (o && o.mode === "raw") Object.assign(pmRequest.body, { mode: "raw", raw: str(o.raw === undefined ? "" : o.raw) });
    },
  });
  let urlObject = new Url(req.url);
  Object.defineProperty(pmRequest, "url", {
    enumerable: true,
    get: () => urlObject,
    set: (v) => {
      urlObject = new Url(typeof v === "string" ? v : v && v.raw !== undefined ? v.raw : String(v));
    },
  });

  // ---- expect ----
  class AssertionError extends Error {
    constructor(message) {
      super(message);
      this.name = "AssertionError";
    }
  }
  const deepEqual = (a, b) => {
    if (a === b) return true;
    if (typeof a === "number" && typeof b === "number") return a !== a && b !== b;
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
    if (Array.isArray(a) !== Array.isArray(b)) return false;
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    return (
      ka.length === kb.length &&
      ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]))
    );
  };
  const typeName = (v) => {
    if (v === null) return "null";
    if (Array.isArray(v)) return "array";
    if (v instanceof Date) return "date";
    if (v instanceof RegExp) return "regexp";
    return typeof v;
  };
  function expect(subject, message) {
    const flags = { not: false, deep: false, any: false };
    const check = (ok, pos, neg) => {
      if (flags.not ? ok : !ok) {
        throw new AssertionError((message ? message + ": " : "") + (flags.not ? neg : pos));
      }
    };
    const a = {};
    ["to", "be", "been", "is", "that", "which", "and", "has", "have", "with", "at", "of", "same", "but", "does", "still", "also", "all"].forEach(
      (w) => Object.defineProperty(a, w, { get: () => a }),
    );
    Object.defineProperty(a, "not", { get: () => ((flags.not = !flags.not), a) });
    Object.defineProperty(a, "deep", { get: () => ((flags.deep = true), a) });
    Object.defineProperty(a, "any", { get: () => ((flags.any = true), a) });
    const flag = (name, test, pos, neg) =>
      Object.defineProperty(a, name, {
        get: () => {
          check(test(), pos, neg);
          return a;
        },
      });
    flag("ok", () => !!subject, `expected ${show(subject)} to be truthy`, `expected ${show(subject)} to be falsy`);
    flag("true", () => subject === true, `expected ${show(subject)} to be true`, `expected ${show(subject)} not to be true`);
    flag("false", () => subject === false, `expected ${show(subject)} to be false`, `expected ${show(subject)} not to be false`);
    flag("null", () => subject === null, `expected ${show(subject)} to be null`, `expected ${show(subject)} not to be null`);
    flag("undefined", () => subject === undefined, `expected ${show(subject)} to be undefined`, `expected ${show(subject)} not to be undefined`);
    flag("NaN", () => subject !== subject, `expected ${show(subject)} to be NaN`, `expected ${show(subject)} not to be NaN`);
    flag("exist", () => subject !== null && subject !== undefined, `expected ${show(subject)} to exist`, `expected ${show(subject)} not to exist`);
    flag(
      "empty",
      () =>
        typeof subject === "string" || Array.isArray(subject)
          ? subject.length === 0
          : subject && typeof subject === "object"
            ? Object.keys(subject).length === 0
            : false,
      `expected ${show(subject)} to be empty`,
      `expected ${show(subject)} not to be empty`,
    );
    const method = (names, fn) =>
      names.forEach((n) => {
        a[n] = (...args) => {
          fn(...args);
          return a;
        };
      });
    method(["equal", "equals", "eq"], (v) =>
      check(
        flags.deep ? deepEqual(subject, v) : subject === v,
        `expected ${show(subject)} to equal ${show(v)}`,
        `expected ${show(subject)} not to equal ${show(v)}`,
      ),
    );
    method(["eql", "eqls"], (v) =>
      check(deepEqual(subject, v), `expected ${show(subject)} to deeply equal ${show(v)}`, `expected ${show(subject)} not to deeply equal ${show(v)}`),
    );
    method(["include", "includes", "contain", "contains"], (v) => {
      let ok;
      if (typeof subject === "string") ok = subject.indexOf(String(v)) >= 0;
      else if (Array.isArray(subject)) ok = flags.deep ? subject.some((x) => deepEqual(x, v)) : subject.indexOf(v) >= 0;
      else if (subject && typeof subject === "object" && v && typeof v === "object")
        ok = Object.keys(v).every((k) => (flags.deep ? deepEqual(subject[k], v[k]) : subject[k] === v[k]));
      else ok = false;
      check(ok, `expected ${show(subject)} to include ${show(v)}`, `expected ${show(subject)} not to include ${show(v)}`);
    });
    method(["above", "greaterThan", "gt"], (n) => check(subject > n, `expected ${show(subject)} to be above ${n}`, `expected ${show(subject)} not to be above ${n}`));
    method(["least", "gte"], (n) => check(subject >= n, `expected ${show(subject)} to be at least ${n}`, `expected ${show(subject)} not to be at least ${n}`));
    method(["below", "lessThan", "lt"], (n) => check(subject < n, `expected ${show(subject)} to be below ${n}`, `expected ${show(subject)} not to be below ${n}`));
    method(["most", "lte"], (n) => check(subject <= n, `expected ${show(subject)} to be at most ${n}`, `expected ${show(subject)} not to be at most ${n}`));
    method(["within"], (lo, hi) => check(subject >= lo && subject <= hi, `expected ${show(subject)} to be within ${lo}..${hi}`, `expected ${show(subject)} not to be within ${lo}..${hi}`));
    method(["closeTo"], (n, d) => check(Math.abs(subject - n) <= d, `expected ${show(subject)} to be close to ${n} +/- ${d}`, `expected ${show(subject)} not to be close to ${n} +/- ${d}`));
    method(["a", "an"], (t) => check(typeName(subject) === String(t).toLowerCase(), `expected ${show(subject)} to be a ${t}`, `expected ${show(subject)} not to be a ${t}`));
    method(["instanceof", "instanceOf"], (c) => check(subject instanceof c, `expected ${show(subject)} to be an instance of ${c && c.name}`, `expected ${show(subject)} not to be an instance of ${c && c.name}`));
    method(["match"], (re) => check(re.test(String(subject)), `expected ${show(subject)} to match ${re}`, `expected ${show(subject)} not to match ${re}`));
    method(["string"], (s) => check(String(subject).indexOf(s) >= 0, `expected ${show(subject)} to contain ${show(s)}`, `expected ${show(subject)} not to contain ${show(s)}`));
    method(["oneOf"], (list) => check(list.indexOf(subject) >= 0, `expected ${show(subject)} to be one of ${show(list)}`, `expected ${show(subject)} not to be one of ${show(list)}`));
    method(["satisfy"], (fn) => check(!!fn(subject), `expected ${show(subject)} to satisfy the function`, `expected ${show(subject)} not to satisfy the function`));
    method(["lengthOf", "length"], (n) => check(subject != null && subject.length === n, `expected ${show(subject)} to have length ${n}`, `expected ${show(subject)} not to have length ${n}`));
    method(["keys", "key"], (...ks) => {
      const wanted = Array.isArray(ks[0]) ? ks[0] : ks;
      const have = subject && typeof subject === "object" ? Object.keys(subject) : [];
      const ok = flags.any ? wanted.some((k) => have.indexOf(k) >= 0) : wanted.every((k) => have.indexOf(k) >= 0) && have.length === wanted.length;
      check(ok, `expected ${show(subject)} to have keys ${show(wanted)}`, `expected ${show(subject)} not to have keys ${show(wanted)}`);
    });
    method(["members"], (list) =>
      check(
        Array.isArray(subject) && subject.length === list.length && list.every((x) => subject.some((y) => deepEqual(x, y))),
        `expected ${show(subject)} to have members ${show(list)}`,
        `expected ${show(subject)} not to have members ${show(list)}`,
      ),
    );
    method(["throw", "throws"], (kind, text) => {
      let thrown;
      try {
        subject();
      } catch (e) {
        thrown = e;
      }
      const ok =
        thrown !== undefined &&
        (kind === undefined || (typeof kind === "function" ? thrown instanceof kind : true)) &&
        (text === undefined || String(thrown && thrown.message).indexOf(String(text)) >= 0);
      check(ok, "expected the function to throw", "expected the function not to throw");
    });
    a.property = (name, ...rest) => {
      const hasValue = rest.length > 0;
      const value = rest[0];
      const has = subject !== null && subject !== undefined && name in Object(subject);
      const matches = !hasValue ? true : flags.deep ? deepEqual(subject[name], value) : subject[name] === value;
      check(
        has && matches,
        `expected ${show(subject)} to have property ${show(name)}${hasValue ? " of " + show(value) : ""}`,
        `expected ${show(subject)} not to have property ${show(name)}`,
      );
      return flags.not ? a : expect(subject[name], message);
    };
    return a;
  }

  // ---- tests ----
  const tests = [];
  const failure = (e) => (e && e.message !== undefined ? String(e.message) : String(e));
  function pmTest(name, fn) {
    const record = { name: String(name), passed: true };
    tests.push(record);
    try {
      const result = typeof fn === "function" ? fn() : undefined;
      if (result && typeof result.then === "function") {
        result.then(
          () => {},
          (e) => {
            record.passed = false;
            record.error = failure(e);
          },
        );
      }
    } catch (e) {
      record.passed = false;
      record.error = failure(e);
    }
  }
  pmTest.skip = (name) => tests.push({ name: String(name), passed: true, skipped: true });

  // ---- response ----
  const resp = input.response;
  let pmResponse;
  if (resp) {
    const headers = new Headers(resp.headers);
    const fail = (m) => {
      throw new AssertionError(m);
    };
    const to = { have: {}, be: {} };
    to.have.status = (v) => {
      const ok = typeof v === "number" ? resp.code === v : String(v).toLowerCase() === resp.status.toLowerCase();
      if (!ok) fail(`expected response to have status ${v} but got ${resp.code}`);
    };
    to.have.header = (k, v) => {
      const got = headers.get(k);
      if (got === undefined) fail(`expected response to have header ${k}`);
      if (v !== undefined && got !== v) fail(`expected header ${k} to be ${v} but got ${got}`);
    };
    to.have.body = (v) => {
      if (v !== undefined && resp.body !== v) fail("expected response body to match");
    };
    to.have.jsonBody = (path, value) => {
      let body;
      try {
        body = JSON.parse(resp.body);
      } catch (e) {
        fail("expected response to have a JSON body");
      }
      if (path !== undefined) {
        const found = String(path).split(".").reduce((o, k) => (o == null ? undefined : o[k]), body);
        if (found === undefined) fail(`expected JSON body to have ${path}`);
        if (value !== undefined && !deepEqual(found, value)) fail(`expected ${path} to be ${show(value)}`);
      }
    };
    const kind = (name, test, text) =>
      Object.defineProperty(to.be, name, {
        get: () => {
          if (!test(resp.code)) fail(`expected response to be ${text} but got ${resp.code}`);
          return to.be;
        },
      });
    kind("ok", (c) => c === 200, "ok (200)");
    kind("success", (c) => c >= 200 && c < 300, "a success (2xx)");
    kind("info", (c) => c >= 100 && c < 200, "informational (1xx)");
    kind("redirection", (c) => c >= 300 && c < 400, "a redirection (3xx)");
    kind("clientError", (c) => c >= 400 && c < 500, "a client error (4xx)");
    kind("serverError", (c) => c >= 500, "a server error (5xx)");
    kind("error", (c) => c >= 400, "an error (4xx/5xx)");
    pmResponse = {
      code: resp.code,
      status: resp.status,
      responseTime: resp.timeMs,
      responseSize: resp.size,
      headers,
      text: () => resp.body,
      json: () => JSON.parse(resp.body),
      to,
    };
  }

  // ---- pm, postman, legacy ----
  const unsupported = (what) => () => {
    throw new Error(`${what} is not supported in Monocode scripts yet`);
  };
  g.pm = {
    info: { eventName: input.event, requestName: input.requestName, requestId: "", iteration: 0, iterationCount: 1 },
    environment,
    collectionVariables: collection,
    globals,
    variables,
    request: pmRequest,
    response: pmResponse,
    cookies: { get: () => undefined, has: () => false, toObject: () => ({}) },
    iterationData: { get: () => undefined, has: () => false, toObject: () => ({}) },
    execution: { setNextRequest: () => {}, skipRequest: () => {} },
    test: pmTest,
    expect,
    sendRequest: unsupported("pm.sendRequest"),
  };
  g.expect = expect;
  g.require = (name) => {
    throw new Error(`require('${name}') is not supported in Monocode scripts yet`);
  };
  g.postman = {
    setEnvironmentVariable: (k, v) => environment.set(k, v),
    getEnvironmentVariable: (k) => environment.get(k),
    clearEnvironmentVariable: (k) => environment.unset(k),
    clearEnvironmentVariables: () => environment.clear(),
    setGlobalVariable: (k, v) => globals.set(k, v),
    getGlobalVariable: (k) => globals.get(k),
    clearGlobalVariable: (k) => globals.unset(k),
    clearGlobalVariables: () => globals.clear(),
    setNextRequest: () => {},
    getResponseHeader: (k) => (pmResponse ? pmResponse.headers.get(k) : undefined),
  };
  g.tests = new Proxy(
    {},
    {
      set(_, name, value) {
        tests.push({ name: String(name), passed: !!value });
        return true;
      },
    },
  );
  if (resp) {
    g.responseBody = resp.body;
    g.responseCode = { code: resp.code, name: resp.status, detail: resp.status };
    g.responseTime = resp.timeMs;
  }
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  g.btoa = (s) => {
    s = String(s);
    let out = "";
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i);
      const b = s.charCodeAt(i + 1);
      const c = s.charCodeAt(i + 2);
      if (a > 255 || b > 255 || c > 255) throw new Error("btoa: the string has characters outside Latin1");
      out += B64[a >> 2] + B64[((a & 3) << 4) | ((b || 0) >> 4)];
      out += i + 1 < s.length ? B64[((b & 15) << 2) | ((c || 0) >> 6)] : "=";
      out += i + 2 < s.length ? B64[c & 63] : "=";
    }
    return out;
  };
  g.atob = (s) => {
    s = String(s).replace(/=+$/, "");
    let out = "";
    let bits = 0;
    let acc = 0;
    for (const ch of s) {
      const v = B64.indexOf(ch);
      if (v < 0) throw new Error("atob: invalid character");
      acc = (acc << 6) | v;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((acc >> bits) & 255);
      }
    }
    return out;
  };

  g.__exec = (code) => {
    try {
      (0, eval)(code);
      return null;
    } catch (e) {
      return e && e.name ? `${e.name}: ${e.message}` : String(e);
    }
  };
  g.__out = () =>
    JSON.stringify({
      environment: environment.entries(),
      collection: collection.entries(),
      globals: globals.entries(),
      local: local.entries(),
      request: {
        method: String(pmRequest.method),
        url: String(pmRequest.url),
        headers: pmRequest.headers.list.map((h) => [h.key, h.value]),
        body: pmRequest.body.raw === undefined ? null : String(pmRequest.body.raw),
      },
      tests,
      logs,
    });
})();
