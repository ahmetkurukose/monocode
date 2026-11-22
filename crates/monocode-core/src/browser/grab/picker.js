// Element grab (context.md K49), run in an isolated world of the page's
// main frame: the page's scripts cannot see or change it. Defines
// `window.__monocodeGrab`: `capture(node)` describes an element; `arm()`
// shows the picker overlay; `wait()` resolves with "picked" or "cancelled";
// `cancel()` and `teardown()` end it. Secrets are left out here and redacted
// again in Rust (monocode_core::browser::grab).
(function () {
  'use strict';
  if (window.__monocodeGrab) {
    try { window.__monocodeGrab.teardown(); } catch (e) {}
  }

  var TEXT_MAX = 200;
  var NEARBY_MAX = 8;
  var SELECTOR_MAX = 700;
  var HTML_MAX = 4096;
  var SAFE_ATTRS = ['id', 'class', 'name', 'type', 'role', 'href', 'src', 'alt',
    'title', 'placeholder', 'for', 'action', 'method', 'disabled', 'checked'];
  var SECRET = /token|secret|passw|passwd|api[-_]?key|auth|csrf|xsrf|session|credential|private|signature|nonce|otp/i;
  var STYLES = ['display', 'position', 'width', 'height', 'margin', 'padding', 'color',
    'background-color', 'border', 'border-radius', 'font-family', 'font-size',
    'font-weight', 'line-height', 'text-align', 'z-index', 'opacity', 'cursor'];

  function clamp(s, max) {
    s = String(s || '');
    return s.length <= max ? s : s.slice(0, max) + '…';
  }

  function squash(s) {
    return String(s || '').replace(/\s+/g, ' ').trim();
  }

  function text(el, max) {
    try {
      var out = '';
      var walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      var node, seen = 0;
      while ((node = walker.nextNode()) && out.length < max + 20 && seen < 80) {
        seen++;
        var parent = node.parentElement;
        if (parent && /^(SCRIPT|STYLE|NOSCRIPT|TEXTAREA)$/.test(parent.tagName)) continue;
        out += ' ' + node.nodeValue;
      }
      return clamp(squash(out), max);
    } catch (e) {
      return '';
    }
  }

  function esc(value) {
    return window.CSS && CSS.escape ? CSS.escape(value) : String(value).replace(/[^\w-]/g, '\\$&');
  }

  // Build-tool hashes make poor selectors.
  function hashy(cls) {
    return /^css-[a-z0-9]+$/i.test(cls) || /^sc-/.test(cls) ||
      (/^[A-Za-z0-9_-]{8,}$/.test(cls) && /\d/.test(cls) && /[A-Z]/.test(cls)) ||
      /__[a-zA-Z0-9]{5,}$/.test(cls);
  }

  function classes(el, max) {
    var out = [];
    var list = el.classList || [];
    for (var i = 0; i < list.length && out.length < max; i++) {
      var c = list[i];
      if (c && c.length <= 60 && !SECRET.test(c) && !hashy(c)) out.push(c);
    }
    return out;
  }

  function unique(selector) {
    try { return document.querySelectorAll(selector).length === 1; } catch (e) { return false; }
  }

  function part(el) {
    var tag = el.tagName.toLowerCase();
    if (el.id && !SECRET.test(el.id) && !hashy(el.id)) return tag + '#' + esc(el.id);
    var testId = el.getAttribute('data-testid') || el.getAttribute('data-test');
    if (testId && !SECRET.test(testId)) {
      return tag + '[data-testid="' + testId.replace(/"/g, '\\"') + '"]';
    }
    return tag + classes(el, 2).map(function (c) { return '.' + esc(c); }).join('');
  }

  function nth(el) {
    var i = 1, s = el.previousElementSibling;
    while (s) { if (s.tagName === el.tagName) i++; s = s.previousElementSibling; }
    if (i > 1) return ':nth-of-type(' + i + ')';
    s = el.nextElementSibling;
    while (s) { if (s.tagName === el.tagName) return ':nth-of-type(1)'; s = s.nextElementSibling; }
    return '';
  }

  // The shortest chain of parts, from the element up, that matches only it.
  function selector(el) {
    var parts = [];
    var cur = el;
    while (cur && cur.nodeType === 1 && cur !== document.documentElement && parts.length < 12) {
      var p = part(cur);
      if (cur.parentElement && !unique([p].concat(parts).join(' > '))) p += nth(cur);
      parts.unshift(p);
      var sel = parts.join(' > ');
      if (unique(sel)) return clamp(sel, SELECTOR_MAX);
      cur = cur.parentElement;
    }
    return clamp(parts.join(' > '), SELECTOR_MAX);
  }

  function path(el) {
    var out = [];
    var cur = el.parentElement;
    while (cur && cur !== document.body && cur !== document.documentElement && out.length < 8) {
      var role = cur.getAttribute('role');
      out.unshift(cur.tagName.toLowerCase() + (role ? '[role=' + role + ']' : ''));
      cur = cur.parentElement;
    }
    return out;
  }

  function nearby(el) {
    var out = [];
    var prev = el.previousElementSibling, next = el.nextElementSibling, seen = 0;
    while ((prev || next) && out.length < NEARBY_MAX && seen < 60) {
      [prev, next].forEach(function (s) {
        if (!s || out.length >= NEARBY_MAX) return;
        seen++;
        var t = text(s, 120);
        if (t) out.push(t);
      });
      prev = prev && prev.previousElementSibling;
      next = next && next.nextElementSibling;
    }
    if (out.length === 0 && el.parentElement) {
      var t = text(el.parentElement, TEXT_MAX);
      if (t) out.push(t);
    }
    return out;
  }

  function secretAttr(name) {
    return name === 'value' || SECRET.test(name);
  }

  function attributes(el) {
    var out = [];
    for (var i = 0; i < el.attributes.length; i++) {
      var a = el.attributes[i];
      var name = a.name.toLowerCase();
      var safe = SAFE_ATTRS.indexOf(name) !== -1 || name.indexOf('aria-') === 0 ||
        name.indexOf('data-') === 0;
      if (!safe) continue;
      out.push([name, secretAttr(name) ? '[redacted]' : clamp(a.value, 300)]);
    }
    return out;
  }

  function styles(el) {
    var cs = getComputedStyle(el);
    return STYLES.map(function (p) { return [p, cs.getPropertyValue(p)]; })
      .filter(function (kv) { return kv[1] !== ''; });
  }

  // The element's HTML without what users typed or secrets: values,
  // password and hidden fields, secret-looking attributes, scripts.
  function html(el) {
    try {
      var copy = el.cloneNode(true);
      var all = [copy].concat(Array.prototype.slice.call(copy.querySelectorAll('*')));
      all.forEach(function (n) {
        var tag = n.tagName;
        if (tag === 'SCRIPT' || tag === 'STYLE') { n.textContent = ''; }
        if (tag === 'TEXTAREA') { n.textContent = ''; }
        for (var i = n.attributes.length - 1; i >= 0; i--) {
          var name = n.attributes[i].name.toLowerCase();
          if (secretAttr(name) || /^on/.test(name)) n.setAttribute(name, '[redacted]');
        }
      });
      var out = copy.outerHTML;
      return out.length <= HTML_MAX * 2 ? out : out.slice(0, HTML_MAX * 2);
    } catch (e) {
      return '';
    }
  }

  function inShadow(el) {
    var root = el.getRootNode && el.getRootNode();
    return !!(root && root !== document && root.host);
  }

  function capture(node) {
    var el = node && node.nodeType === 3 ? node.parentElement : node;
    if (!el || el.nodeType !== 1) return null;
    var rect = el.getBoundingClientRect();
    var fixed = false;
    for (var cur = el; cur && cur !== document.body; cur = cur.parentElement) {
      var pos = getComputedStyle(cur).position;
      if (pos === 'fixed' || pos === 'sticky') { fixed = true; break; }
    }
    var tag = el.tagName.toLowerCase();
    var type = (el.getAttribute('type') || '').toLowerCase();
    var field = /^(input|textarea|select)$/.test(tag);
    return {
      url: location.href,
      title: document.title || '',
      tag: tag,
      selector: selector(el),
      path: path(el),
      in_shadow: inShadow(el),
      role: el.getAttribute('role') || '',
      label: el.getAttribute('aria-label') || '',
      // What users typed stays out; a field says only that it has a value.
      text: field ? '' : text(el, TEXT_MAX),
      field: field ? { type: type || tag, filled: !!(el.value && String(el.value).length) } : null,
      nearby: nearby(el),
      attributes: attributes(el),
      styles: styles(el),
      html: html(el),
      fixed: fixed,
      rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      viewport: {
        width: window.innerWidth,
        height: window.innerHeight,
        scroll_x: window.scrollX,
        scroll_y: window.scrollY,
        ratio: window.devicePixelRatio || 1
      }
    };
  }

  // ---- The picker overlay ----

  var host = null, box = null, label = null, top = null, current = null;
  var done = null, result = null, listeners = [];

  function listen(target, type, fn) {
    target.addEventListener(type, fn, true);
    listeners.push([target, type, fn]);
  }

  function show(el) {
    if (!el || el === document.documentElement || el === document.body || el === host) {
      box.style.display = 'none';
      label.style.display = 'none';
      current = null;
      return;
    }
    current = el;
    var r = el.getBoundingClientRect();
    box.style.display = 'block';
    box.style.left = r.x + 'px';
    box.style.top = r.y + 'px';
    box.style.width = r.width + 'px';
    box.style.height = r.height + 'px';
    var bits = [el.tagName.toLowerCase()];
    var role = el.getAttribute('role');
    if (role) bits.push('role=' + role);
    var t = text(el, 40);
    if (t) bits.push('"' + t + '"');
    bits.push(Math.round(r.width) + '×' + Math.round(r.height));
    label.textContent = bits.join('  ');
    var y = r.bottom + 6;
    if (y + 28 > window.innerHeight) y = Math.max(4, r.top - 28);
    label.style.display = 'block';
    label.style.left = Math.max(4, r.x) + 'px';
    label.style.top = y + 'px';
  }

  function under(x, y) {
    host.style.pointerEvents = 'none';
    var el = document.elementFromPoint(x, y);
    try {
      while (el && el.shadowRoot) {
        var inner = el.shadowRoot.elementFromPoint(x, y);
        if (!inner || inner === el) break;
        el = inner;
      }
    } catch (e) {
    } finally {
      host.style.pointerEvents = 'auto';
    }
    return el;
  }

  function finish(how) {
    if (result) return;
    result = how;
    teardown();
    if (done) done(how);
  }

  function arm() {
    teardown();
    result = null;
    api.picked = null;
    host = document.createElement('div');
    host.style.cssText = 'position:fixed;inset:0;z-index:2147483647;pointer-events:auto;cursor:crosshair;background:transparent;';
    var shadow = host.attachShadow({ mode: 'closed' });
    // The top layer keeps the outline above modal dialogs and popovers.
    top = document.createElement('div');
    top.setAttribute('popover', 'manual');
    top.style.cssText = 'position:fixed;inset:0;width:100vw;height:100vh;max-width:none;max-height:none;margin:0;padding:0;border:0;background:transparent;overflow:visible;pointer-events:none;';
    box = document.createElement('div');
    box.style.cssText = 'position:fixed;display:none;pointer-events:none;box-sizing:border-box;border:2px solid #3b82f6;border-radius:3px;background:rgba(59,130,246,0.12);box-shadow:0 0 0 1px rgba(255,255,255,0.6);';
    label = document.createElement('div');
    label.style.cssText = 'position:fixed;display:none;pointer-events:none;padding:3px 8px;border-radius:4px;background:rgba(24,24,27,0.92);color:#f4f4f5;font:11px/1.4 -apple-system,system-ui,sans-serif;white-space:nowrap;max-width:320px;overflow:hidden;text-overflow:ellipsis;';
    top.appendChild(box);
    top.appendChild(label);
    shadow.appendChild(top);
    document.documentElement.appendChild(host);
    try { top.showPopover(); } catch (e) {}
    listen(window, 'mousemove', function (e) { show(under(e.clientX, e.clientY)); });
    // The page never sees the press that picks.
    ['pointerdown', 'mousedown', 'pointerup', 'mouseup', 'click', 'dblclick', 'auxclick', 'contextmenu']
      .forEach(function (type) {
        listen(window, type, function (e) {
          if (!e.isTrusted) return;
          e.preventDefault();
          e.stopImmediatePropagation();
          if (type === 'click' && e.button === 0) {
            var el = under(e.clientX, e.clientY);
            if (el && el !== document.documentElement) {
              api.picked = el;
              finish('picked');
            }
          }
        });
      });
    listen(window, 'keydown', function (e) {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopImmediatePropagation();
        finish('cancelled');
      }
    });
    // A later top-layer element covers the outline until it is shown again.
    listen(window, 'toggle', function () {
      try { top.hidePopover(); top.showPopover(); } catch (e) {}
    });
    listen(window, 'pagehide', function () { finish('cancelled'); });
    return true;
  }

  function teardown() {
    listeners.forEach(function (l) { l[0].removeEventListener(l[1], l[2], true); });
    listeners = [];
    if (host) { try { host.remove(); } catch (e) {} }
    host = box = label = top = current = null;
  }

  var api = window.__monocodeGrab = {
    picked: null,
    capture: capture,
    arm: arm,
    wait: function () {
      if (result) return Promise.resolve(result);
      return new Promise(function (resolve) { done = resolve; });
    },
    cancel: function () { finish('cancelled'); return true; },
    teardown: teardown
  };
  return true;
})()
