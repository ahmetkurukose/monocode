// Called on the grabbed element in the page's own world, where React keeps
// its fiber on the node: the components around it, innermost last, and the
// source file of the nearest one that has it (development builds only).
function () {
  var el = this && this.nodeType === 3 ? this.parentElement : this;
  if (!el) return null;
  var fiber = null;
  try {
    var keys = Object.keys(el);
    for (var i = 0; i < keys.length; i++) {
      if (keys[i].indexOf('__reactFiber$') === 0 || keys[i].indexOf('__reactInternalInstance$') === 0) {
        fiber = el[keys[i]];
        break;
      }
    }
  } catch (e) {
    return null;
  }
  if (!fiber) return null;
  function name(f) {
    var t = f.type || f.elementType;
    if (!t || typeof t === 'string') return null;
    return t.displayName || t.name ||
      (t.render && (t.render.displayName || t.render.name)) ||
      (t.type && (t.type.displayName || t.type.name)) || null;
  }
  function skip(n) {
    return !n || n.length <= 2 ||
      /^(Fragment|Root|Routes|Route|Outlet|Provider|Consumer|Profiler|Suspense|StrictMode)$/.test(n) ||
      /(Boundary|Router|Provider|Consumer|Context)$/.test(n);
  }
  var components = [];
  var source = null;
  for (var depth = 0; fiber && depth < 40; depth++, fiber = fiber.return) {
    try {
      var n = name(fiber);
      if (!skip(n) && components.indexOf(n) === -1 && components.length < 6) components.push(n);
      var s = fiber._debugSource || (fiber._debugOwner && fiber._debugOwner._debugSource) ||
        (fiber.memoizedProps && fiber.memoizedProps.__source);
      if (!source && s && s.fileName) {
        source = String(s.fileName) + (s.lineNumber ? ':' + s.lineNumber : '') +
          (s.lineNumber && s.columnNumber ? ':' + s.columnNumber : '');
      }
    } catch (e) {}
  }
  return { components: components.reverse(), source: source };
}
