// The password watcher (context.md K56, K57): runs in an isolated world of
// every document of a page, apart from the page's own scripts, which can
// neither see nor call it. It reports through the world's binding:
//
//   {t:"focus", kind:"username"|"password", field, rect, top}
//   {t:"blur", field}
//   {t:"submit", url, username, password} a form was sent with a password
//   {t:"username", url, username}         a user name was sent on its own
//   {t:"gone"}                            the sent form's password went away
//   {t:"forms", password}                 whether the document shows one
//
// and fills saved logins on request: `__monocodePasswords.fill(user, pw,
// field)`. Fields are named by random tokens kept here, never written onto
// the page's nodes.
(function () {
  if (globalThis.__monocodePasswords) return;
  // Without the binding (a world made later to fill) nothing is reported.
  var report = globalThis.__monocodePasswordsReport;
  function send(message) {
    if (typeof report !== "function") return;
    try { report(JSON.stringify(message)); } catch (e) {}
  }
  var top = window === window.top;

  var tokens = new WeakMap();
  var fields = new Map();
  function token(el) {
    var t = tokens.get(el);
    if (!t) {
      t = "f" + Math.random().toString(36).slice(2) + Date.now().toString(36);
      tokens.set(el, t);
      fields.set(t, new WeakRef(el));
    }
    return t;
  }
  function byToken(t) {
    var ref = t && fields.get(t);
    var el = ref && ref.deref();
    return el && el.isConnected ? el : null;
  }

  var TEXTY = { "": 1, text: 1, email: 1, tel: 1, url: 1, search: 0 };
  var OTP = /one-time|otp|totp|2fa|mfa|verification|security.?code|sms|pin\b/i;
  var USERISH = /user|login|email|e-mail|account|identifier|phone|mail/i;
  // Fields that once were password fields (a "show password" toggle).
  var wasPassword = new WeakSet();

  function isInput(el) {
    return el && el.tagName === "INPUT";
  }
  function isPassword(el) {
    if (!isInput(el)) return false;
    var type = (el.getAttribute("type") || "").toLowerCase();
    if (type === "password") { wasPassword.add(el); return true; }
    return wasPassword.has(el) && type === "text";
  }
  function attrs(el) {
    return [el.name, el.id, el.getAttribute("autocomplete"), el.getAttribute("aria-label"),
      el.getAttribute("placeholder")].join(" ");
  }
  function isOtp(el) {
    return (el.getAttribute("autocomplete") || "") === "one-time-code" || OTP.test(attrs(el));
  }
  function isTexty(el) {
    if (!isInput(el) || isPassword(el)) return false;
    var type = (el.getAttribute("type") || "").toLowerCase();
    return TEXTY[type] === 1 && !isOtp(el);
  }
  function visible(el) {
    if (!el || !el.isConnected || el.disabled) return false;
    var rect = el.getBoundingClientRect();
    if (rect.width < 2 || rect.height < 2) return false;
    var style = getComputedStyle(el);
    return style.visibility !== "hidden" && style.display !== "none" && style.opacity !== "0";
  }
  // The inputs of a field's group: its form, or the document's inputs
  // outside any form. Open shadow roots are walked.
  function inputs(root) {
    var out = [];
    (function walk(node) {
      var list = node.querySelectorAll("input");
      for (var i = 0; i < list.length; i++) out.push(list[i]);
      var all = node.querySelectorAll("*");
      for (var j = 0; j < all.length && j < 3000; j++) {
        if (all[j].shadowRoot) walk(all[j].shadowRoot);
      }
    })(root);
    return out;
  }
  function groupOf(el) {
    var form = el.form || (el.closest && el.closest("form"));
    if (form) return { form: form, inputs: inputs(form) };
    return {
      form: null,
      inputs: inputs(document).filter(function (i) { return !i.form && !i.closest("form"); }),
    };
  }
  // A group's fields: its visible password fields, and the user name
  // field: the one that says so, else the last text field before the first
  // password field.
  function parse(group) {
    var passwords = group.inputs.filter(function (i) { return isPassword(i) && visible(i); });
    var texts = group.inputs.filter(function (i) { return isTexty(i) && visible(i); });
    var username = null;
    for (var i = 0; i < texts.length; i++) {
      var ac = (texts[i].getAttribute("autocomplete") || "").toLowerCase();
      if (ac.indexOf("username") >= 0 || ac === "email") { username = texts[i]; break; }
    }
    if (!username && passwords.length) {
      var first = passwords[0];
      for (var k = 0; k < texts.length; k++) {
        if (texts[k].compareDocumentPosition(first) & Node.DOCUMENT_POSITION_FOLLOWING) {
          username = texts[k];
        }
      }
    }
    if (!username && !passwords.length) {
      for (var m = 0; m < texts.length; m++) {
        var t = texts[m];
        if ((t.getAttribute("type") || "") === "email" || USERISH.test(attrs(t))) { username = t; break; }
      }
    }
    return { passwords: passwords, username: username };
  }
  // The password a sent form signs in with: a new one when it sets one
  // (sign-up, change), else the first.
  function chosenPassword(passwords) {
    var filled = passwords.filter(function (p) { return p.value; });
    if (!filled.length) return null;
    for (var i = 0; i < filled.length; i++) {
      if ((filled[i].getAttribute("autocomplete") || "").indexOf("new-password") >= 0) return filled[i];
    }
    if (filled.length === 3) return filled[1];
    return filled[0];
  }

  // ---- Sending ----
  var watching = null;
  function capture(el) {
    var group = groupOf(el);
    var parsed = parse(group);
    var password = chosenPassword(parsed.passwords);
    var username = parsed.username ? parsed.username.value : "";
    if (!password) {
      if (username && !parsed.passwords.length) {
        send({ t: "username", url: location.href, username: username });
      }
      return;
    }
    send({ t: "submit", url: location.href, username: username, password: password.value });
    watchGone(password);
  }
  function watchGone(field) {
    if (watching) clearInterval(watching);
    var started = Date.now();
    watching = setInterval(function () {
      if (!visible(field)) {
        clearInterval(watching);
        watching = null;
        send({ t: "gone" });
      } else if (Date.now() - started > SAME_DOCUMENT_MS) {
        clearInterval(watching);
        watching = null;
      }
    }, 250);
  }
  var lastCapture = 0;
  function captureSoon(el) {
    // A click on the button and the form's submit come together.
    var now = Date.now();
    if (now - lastCapture < 50) return;
    lastCapture = now;
    capture(el);
  }
  addEventListener("submit", function (event) {
    var form = event.target;
    var input = form && form.querySelector && form.querySelector("input");
    if (input) captureSoon(input);
  }, true);
  addEventListener("click", function (event) {
    var target = event.target && event.target.closest &&
      event.target.closest("button, input[type=submit], input[type=button], [role=button]");
    if (!target) return;
    var group = groupOf(target);
    if (!parse(group).passwords.some(function (p) { return p.value; }) && !target.form) return;
    if (target.form && target.type === "submit") return; // The submit event follows.
    captureSoon(target);
  }, true);
  addEventListener("keydown", function (event) {
    if (event.key !== "Enter" || !isInput(event.target)) return;
    if (event.target.form) return; // The submit event follows.
    if (isPassword(event.target) || isTexty(event.target)) captureSoon(event.target);
  }, true);

  // ---- Focus ----
  function rectOf(el) {
    var r = el.getBoundingClientRect();
    return { x: r.left, y: r.top, width: r.width, height: r.height };
  }
  addEventListener("focusin", function (event) {
    var el = event.composedPath ? event.composedPath()[0] : event.target;
    if (!isInput(el)) return;
    var kind = isPassword(el) ? "password" : null;
    if (!kind && isTexty(el)) {
      var parsed = parse(groupOf(el));
      if (parsed.username === el) kind = "username";
    }
    if (!kind) return;
    send({ t: "focus", kind: kind, field: token(el), rect: rectOf(el), top: top });
  }, true);
  addEventListener("focusout", function (event) {
    var el = event.composedPath ? event.composedPath()[0] : event.target;
    if (isInput(el) && tokens.has(el)) send({ t: "blur", field: tokens.get(el) });
  }, true);

  // ---- What the document shows ----
  function showsPassword() {
    var list = inputs(document);
    for (var i = 0; i < list.length; i++) if (isPassword(list[i]) && visible(list[i])) return true;
    return false;
  }
  if (top) {
    var told = null;
    var tell = function () {
      var now = showsPassword();
      if (now !== told) { told = now; send({ t: "forms", password: now }); }
    };
    var later = function () { setTimeout(tell, 0); setTimeout(tell, 1200); setTimeout(tell, 2400); };
    if (document.readyState === "loading") addEventListener("DOMContentLoaded", later, true);
    else later();
    addEventListener("load", function () { setTimeout(tell, 300); }, true);
  }

  // ---- Filling ----
  var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  function put(el, value) {
    el.focus();
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }
  function fill(username, password, field, start) {
    var el = byToken(field) || start || document.activeElement;
    if (!isInput(el) || !(isPassword(el) || isTexty(el))) {
      el = inputs(document).filter(function (i) { return isPassword(i) && visible(i); })[0] || null;
    }
    if (!el) return { ok: false, why: "no password field" };
    var parsed = parse(groupOf(el));
    if (!parsed.passwords.length && !parsed.username) return { ok: false, why: "no login form" };
    var filled = { ok: true, username: false, password: false };
    if (parsed.username && username && !parsed.username.readOnly) {
      put(parsed.username, username);
      filled.username = true;
    }
    var target = parsed.passwords.filter(function (p) { return !p.readOnly; })[0];
    if (target && password) {
      put(target, password);
      filled.password = true;
    }
    if (!filled.username && !filled.password) return { ok: false, why: "the fields cannot be edited" };
    return filled;
  }

  var SAME_DOCUMENT_MS = 6000;
  globalThis.__monocodePasswords = { fill: fill };
})();
