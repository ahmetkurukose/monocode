//! The scripts an agent's tools run in a page (docs/native/browser.md §3.7).
//!
//! Two parts. `HOOKS` is put in every page before its own scripts run and
//! keeps what the page logs and requests, since those cannot be asked for
//! afterwards. `LIBRARY` is sent along with each call: reading the page as
//! an outline with element handles, and acting on a handle.
//!
//! Contract: a handle names exactly one element of one read. It ends with
//! the next read or navigation, and an element that is gone, hidden or
//! covered is an error, never a click on something near it. The value of a
//! password or payment field never leaves the page.

/// Runs at document start in the page's own world: console and requests.
pub(super) const HOOKS: &str = r#"(() => {
  if (window.__monocode) return;
  const M = { console: [], network: [], seq: 0, handles: new Map() };
  const push = (list, entry) => { entry.n = ++M.seq; list.push(entry); if (list.length > 1000) list.shift(); };
  const show = (v) => {
    try {
      if (typeof v === 'string') return v;
      if (v instanceof Error) return v.stack || String(v);
      const json = JSON.stringify(v);
      return json === undefined ? String(v) : json;
    } catch (e) { return String(v); }
  };
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    const original = console[level];
    if (typeof original !== 'function') continue;
    console[level] = function (...args) {
      push(M.console, { level, text: args.map(show).join(' ').slice(0, 4000) });
      return original.apply(this, args);
    };
  }
  addEventListener('error', (e) => {
    const where = e.filename ? ` (${e.filename}:${e.lineno})` : '';
    push(M.console, { level: 'error', text: `Uncaught ${e.message || 'error'}${where}` });
  });
  addEventListener('unhandledrejection', (e) => {
    push(M.console, { level: 'error', text: 'Unhandled rejection: ' + show(e.reason) });
  });
  const fetch0 = window.fetch;
  if (typeof fetch0 === 'function') {
    window.fetch = function (input, init) {
      const started = performance.now();
      const method = (init && init.method) || (input && input.method) || 'GET';
      const url = typeof input === 'string' ? input : (input && input.url) || String(input);
      const entry = { kind: 'fetch', method: String(method).toUpperCase(), url: String(url).slice(0, 2000), status: null };
      push(M.network, entry);
      const done = () => { entry.ms = Math.round(performance.now() - started); };
      return fetch0.apply(this, arguments).then(
        (response) => { entry.status = response.status; done(); return response; },
        (error) => { entry.status = 'failed'; entry.error = String(error).slice(0, 300); done(); throw error; });
    };
  }
  const XHR = window.XMLHttpRequest;
  if (XHR) {
    const open = XHR.prototype.open, send = XHR.prototype.send;
    XHR.prototype.open = function (method, url) {
      this.__monocode = { kind: 'xhr', method: String(method).toUpperCase(), url: String(url).slice(0, 2000), status: null };
      return open.apply(this, arguments);
    };
    XHR.prototype.send = function () {
      const entry = this.__monocode;
      if (entry) {
        const started = performance.now();
        push(M.network, entry);
        this.addEventListener('loadend', () => {
          entry.status = this.status || 'failed';
          entry.ms = Math.round(performance.now() - started);
        });
      }
      return send.apply(this, arguments);
    };
  }
  Object.defineProperty(window, '__monocode', { value: M, enumerable: false });
})();"#;

/// Defines the functions the tools call. Sent before each call's own
/// expression, inside one function scope, so a page never keeps it.
pub(super) const LIBRARY: &str = r#"
const M = window.__monocode || (window.__monocode_late = window.__monocode_late || { console: [], network: [], seq: 0, handles: new Map() });
const squash = (s) => String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
const clip = (s, n) => (s.length > n ? s.slice(0, n - 1) + '…' : s);
const visible = (el) => {
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return false;
  const s = getComputedStyle(el);
  return s.visibility !== 'hidden' && s.display !== 'none';
};
const secret = (el) => {
  const kind = (el.getAttribute && el.getAttribute('type') || '').toLowerCase();
  const fill = (el.getAttribute && el.getAttribute('autocomplete') || '').toLowerCase();
  return kind === 'password' || /cc-|credit|card|cvc|cvv|one-time-code/.test(fill);
};
const roleOf = (el) => {
  const given = el.getAttribute('role');
  if (given) return given;
  const tag = el.tagName.toLowerCase();
  if (tag === 'a') return el.hasAttribute('href') ? 'link' : null;
  if (tag === 'button' || tag === 'summary') return 'button';
  if (tag === 'select') return 'combobox';
  if (tag === 'textarea') return 'textbox';
  if (tag === 'input') {
    const kind = (el.getAttribute('type') || 'text').toLowerCase();
    if (kind === 'hidden') return null;
    if (kind === 'checkbox') return 'checkbox';
    if (kind === 'radio') return 'radio';
    if (kind === 'range') return 'slider';
    if (kind === 'search') return 'searchbox';
    if (['button', 'submit', 'reset', 'image', 'file'].includes(kind)) return 'button';
    return 'textbox';
  }
  if (el.isContentEditable && el.getAttribute('contenteditable') !== null) return 'textbox';
  if (/^h[1-6]$/.test(tag)) return 'heading';
  if (tag === 'img') return 'image';
  return null;
};
const ACTS = new Set(['link', 'button', 'textbox', 'searchbox', 'checkbox', 'radio', 'slider', 'combobox',
  'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'switch', 'treeitem', 'spinbutton']);
const acts = (el, role) => ACTS.has(role) || el.hasAttribute('onclick') || el.getAttribute('tabindex') === '0';
const nameOf = (el, role) => {
  const aria = el.getAttribute('aria-label');
  if (aria) return squash(aria);
  const by = el.getAttribute('aria-labelledby');
  if (by) {
    const text = by.split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean).map((e) => e.innerText).join(' ');
    if (squash(text)) return squash(text);
  }
  const tag = el.tagName.toLowerCase();
  if (tag === 'input' || tag === 'select' || tag === 'textarea') {
    if (el.labels && el.labels.length) return squash([...el.labels].map((l) => l.innerText).join(' '));
    const kind = (el.getAttribute('type') || '').toLowerCase();
    if (['button', 'submit', 'reset'].includes(kind) && el.value) return squash(el.value);
    return squash(el.getAttribute('placeholder') || el.getAttribute('title') || el.getAttribute('name') || '');
  }
  if (role === 'image') return squash(el.getAttribute('alt') || el.getAttribute('title') || '');
  return squash(el.innerText || el.getAttribute('title') || el.getAttribute('alt') || '');
};
const line = (el, role, id) => {
  let out = (id ? `[${id}] ` : '') + role;
  const name = clip(nameOf(el, role), 120);
  if (name) out += ` "${name}"`;
  const tag = el.tagName.toLowerCase();
  if (role === 'heading') out = out.replace('heading', `heading(${tag[1] || el.getAttribute('aria-level') || '?'})`);
  if (role === 'link') { const href = el.getAttribute('href'); if (href) out += ` href=${clip(href, 100)}`; }
  if (role === 'textbox' || role === 'searchbox' || role === 'spinbutton' || role === 'slider') {
    if (secret(el)) out += ' value=(hidden)';
    else if (tag === 'input' || tag === 'textarea') out += ` value="${clip(String(el.value || ''), 200)}"`;
    else out += ` value="${clip(squash(el.innerText), 200)}"`;
  }
  if (role === 'combobox' && tag === 'select') {
    const picked = el.selectedOptions && el.selectedOptions[0];
    out += ` value="${picked ? clip(squash(picked.text), 80) : ''}"`;
    out += ` options=[${[...el.options].slice(0, 12).map((o) => clip(squash(o.text), 40)).join(' | ')}]`;
  }
  if (role === 'checkbox' || role === 'radio' || role === 'switch') {
    const on = tag === 'input' ? el.checked : el.getAttribute('aria-checked') === 'true';
    out += on ? ' checked' : ' unchecked';
  }
  if (el.disabled || el.getAttribute('aria-disabled') === 'true') out += ' disabled';
  if (el.getAttribute('aria-expanded')) out += ` expanded=${el.getAttribute('aria-expanded')}`;
  return out;
};
const fresh = () => { M.handles = new Map(); };
const grant = (el) => { const id = 'e' + (M.handles.size + 1); M.handles.set(id, el); return id; };
const SKIP = new Set(['script', 'style', 'noscript', 'template', 'svg', 'head', 'meta', 'link']);

function outline(limit) {
  fresh();
  const lines = [`page: ${squash(document.title)}`, `url: ${location.href}`];
  let size = 0, cut = false, text = '';
  const say = (s) => { if (cut) return; size += s.length + 1; if (lines.length > 1500 || size > limit) { cut = true; return; } lines.push(s); };
  const flush = () => { const t = squash(text); text = ''; if (t) for (let at = 0; at < t.length; at += 300) say('text "' + t.slice(at, at + 300) + '"'); };
  const walk = (node) => {
    if (cut) return;
    if (node.nodeType === 3) { text += ' ' + node.nodeValue; return; }
    if (node.nodeType !== 1) return;
    const tag = node.tagName.toLowerCase();
    if (SKIP.has(tag) || node.getAttribute('aria-hidden') === 'true') return;
    if (!visible(node)) { if (getComputedStyle(node).display !== 'contents') return; }
    const role = roleOf(node);
    if (role && acts(node, role)) { flush(); say(line(node, role, grant(node))); return; }
    if (!role && acts(node, 'generic')) { flush(); say(line(node, 'clickable', grant(node))); return; }
    if (role === 'heading' || role === 'image') { flush(); const l = line(node, role, null); if (role !== 'image' || l !== 'image') say(l); return; }
    const block = /^(p|div|li|tr|td|th|section|article|header|footer|main|nav|aside|ul|ol|table|form|fieldset|label|dd|dt|dl|blockquote|pre|br|hr)$/.test(tag);
    if (block) flush();
    const root = node.shadowRoot;
    for (const child of (root ? root.childNodes : node.childNodes)) walk(child);
    if (block) flush();
  };
  walk(document.body || document.documentElement);
  flush();
  if (cut) lines.push('(the page goes on: this outline was cut short; use browser_find for the rest)');
  return { text: lines.join('\n'), handles: M.handles.size };
}

function pageText(limit) {
  const text = (document.body ? document.body.innerText : '') || '';
  return { text: `page: ${squash(document.title)}\nurl: ${location.href}\n\n` + (text.length > limit ? text.slice(0, limit) + '\n(cut short)' : text) };
}

function find(q) {
  fresh();
  let pool;
  try { pool = q.selector ? [...document.querySelectorAll(q.selector)] : [...document.querySelectorAll('*')]; }
  catch (e) { throw new Error('That is not a valid CSS selector: ' + q.selector); }
  const want = q.text ? q.text.toLowerCase() : null;
  const lines = [];
  for (const el of pool) {
    if (lines.length >= 50) { lines.push('(more matches were left out)'); break; }
    if (SKIP.has(el.tagName.toLowerCase()) || !visible(el)) continue;
    const role = roleOf(el) || (acts(el, 'generic') ? 'clickable' : null);
    if (q.role && role !== q.role) continue;
    if (want) {
      const name = nameOf(el, role || 'generic').toLowerCase();
      if (!name.includes(want)) continue;
      // The innermost element that carries the text, not every ancestor of it.
      if (!q.selector && !role && [...el.children].some((c) => visible(c) && squash(c.innerText).toLowerCase().includes(want))) continue;
    } else if (!q.selector && !q.role) continue;
    lines.push(line(el, role || el.tagName.toLowerCase(), grant(el)));
  }
  return { text: lines.length ? lines.join('\n') : 'Nothing on the page matches.', handles: M.handles.size };
}

function element(id) {
  const el = M.handles.get(id);
  if (!el) throw new Error(`${id} is not a handle of the last read of this page. Read the page again.`);
  if (!el.isConnected) throw new Error(`${id} is no longer on the page. Read the page again.`);
  return el;
}

function locate(id) {
  const el = element(id);
  el.scrollIntoView({ block: 'center', inline: 'center' });
  if (!visible(el)) throw new Error(`${id} is hidden.`);
  const r = el.getBoundingClientRect();
  const x = Math.min(Math.max(r.left + r.width / 2, 1), innerWidth - 1);
  const y = Math.min(Math.max(r.top + r.height / 2, 1), innerHeight - 1);
  const top = document.elementFromPoint(x, y);
  if (top && top !== el && !el.contains(top) && !top.contains(el)) {
    const what = top.id ? '#' + top.id : top.tagName.toLowerCase();
    throw new Error(`${id} is covered by ${what}.`);
  }
  return { x, y, disabled: !!(el.disabled || el.getAttribute('aria-disabled') === 'true') };
}

function clickByScript(id) { const el = element(id); el.focus && el.focus(); el.click(); return true; }

function typeInto(id, text, clear) {
  const el = element(id);
  if (!visible(el)) throw new Error(`${id} is hidden.`);
  el.focus();
  const tag = el.tagName.toLowerCase();
  const field = tag === 'input' || tag === 'textarea';
  if (clear) { if (field && el.select) el.select(); else document.execCommand('selectAll'); }
  else if (field && el.setSelectionRange) { try { el.setSelectionRange(el.value.length, el.value.length); } catch (e) {} }
  const before = field ? el.value : el.innerText;
  let done = false;
  try { done = document.execCommand('insertText', false, text); } catch (e) {}
  const after = field ? el.value : el.innerText;
  if (field && (!done || (after === before && text !== ''))) {
    const proto = tag === 'input' ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, clear ? text : before + text);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }
  return secret(el) ? '(hidden)' : clip(String(field ? el.value : el.innerText), 500);
}

function pressKey(key, id) {
  const el = id ? element(id) : (document.activeElement || document.body);
  if (id && el.focus) el.focus();
  const init = { key, bubbles: true, cancelable: true, composed: true };
  const go = el.dispatchEvent(new KeyboardEvent('keydown', init));
  if (go && key.length === 1) el.dispatchEvent(new KeyboardEvent('keypress', init));
  // A key sent from script has no default action: the usual ones are done by hand.
  if (go && key === 'Enter') {
    const tag = el.tagName.toLowerCase();
    if ((tag === 'input' || tag === 'select') && el.form) { if (el.form.requestSubmit) el.form.requestSubmit(); else el.form.submit(); }
    else if (tag === 'button' || tag === 'a' || el.getAttribute('role') === 'button') el.click();
  }
  if (go && key === ' ' && ['button', 'checkbox', 'radio', 'switch'].includes(roleOf(el))) el.click();
  el.dispatchEvent(new KeyboardEvent('keyup', init));
  return true;
}

function scrollPage(dy, id) {
  if (id) { element(id).scrollIntoView({ block: 'center', inline: 'center' }); }
  else { (document.scrollingElement || document.documentElement).scrollBy(0, dy || 0); }
  const s = document.scrollingElement || document.documentElement;
  return { y: Math.round(s.scrollTop), height: Math.round(s.scrollHeight), view: innerHeight };
}

function holds(q) {
  const text = () => (document.body ? document.body.innerText : '');
  if (q.text && !text().includes(q.text)) return false;
  if (q.text_gone && text().includes(q.text_gone)) return false;
  if (q.selector) { try { if (!document.querySelector(q.selector)) return false; } catch (e) { throw new Error('That is not a valid CSS selector: ' + q.selector); } }
  return true;
}

function since(list, mark) {
  const entries = list.filter((e) => e.n > mark).map((e) => { const copy = Object.assign({}, e); delete copy.n; return copy; });
  return { entries, next: M.seq };
}
"#;

/// A call's expression with the library in scope.
pub(crate) fn call(expression: &str) -> String {
    format!("(()=>{{{LIBRARY}\nreturn ({expression});}})()")
}

/// A value as a JavaScript literal.
pub(crate) fn literal(value: &serde_json::Value) -> String {
    // JSON is JavaScript but for two line separators that end a statement.
    value
        .to_string()
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_carries_the_library_and_its_arguments_as_literals() {
        let script = call(&format!(
            "typeInto({}, {}, true)",
            literal(&"e3".into()),
            literal(&"a \"b\"\n".into())
        ));
        assert!(script.starts_with("(()=>{"));
        assert!(script.contains("function typeInto("));
        assert!(script.ends_with("return (typeInto(\"e3\", \"a \\\"b\\\"\\n\", true));})()"));
        assert_eq!(literal(&"x\u{2028}y".into()), "\"x\\u2028y\"");
        // The page's world keeps nothing of the library but the handle table.
        assert!(!HOOKS.contains("function outline"));
        assert!(HOOKS.contains("__monocode"));
    }
}
