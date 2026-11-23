//! The scripts the layer runs, made from an extension's manifest:
//!
//! - [`page_shim`]: `mcext://<id>/__mcext/shim.js`, the first script of
//!   every extension page and of the background worker.
//! - [`worker_script`] and [`BACKGROUND_HTML`]: the background service
//!   worker runs as a dedicated worker of a hidden page.
//! - [`content_programs`]: what a web page runs for the extension, injected
//!   over CDP before the page's own scripts: one program for the
//!   extension's isolated world (the shim, then its content scripts by
//!   `run_at`, `all_frames` and matches), one for the main world (its
//!   `world: "MAIN"` scripts, and `external.js` where
//!   `externally_connectable` matches).

use std::path::Path;

use serde_json::{Value, json};

use super::manifest::{ContentScript, Manifest, RunAt, World};

/// The `chrome.*` shim.
pub const SHIM: &str = include_str!("shim.js");
/// Main-world messaging for externally connectable pages.
pub const EXTERNAL: &str = include_str!("external.js");

/// The hidden page the background worker runs in.
pub const BACKGROUND_HTML: &str = "<!doctype html><meta charset=utf-8><title>background</title><script src=\"/__mcext/background-host.js\"></script>";

/// Where the layer's own files live in an extension's origin. Extensions
/// cannot ship files there (`__` names are reserved by Chrome too).
pub const RESERVED: &str = "__mcext/";

/// What every shim config carries.
pub struct ShimConfig<'a> {
    pub id: &'a str,
    pub manifest: &'a Manifest,
    pub locale: &'a str,
    /// Development hook: the context's `chrome` also as `__mcextChrome`
    /// (extensions such as Proton Pass hide `chrome` from code they did not
    /// load, scripts that inspect them included).
    pub expose: bool,
}

impl ShimConfig<'_> {
    fn base(&self, mode: &str) -> Value {
        json!({
            "id": self.id,
            "base": super::base_url(self.id),
            "manifest": self.manifest.raw,
            "messages": self.manifest.messages,
            "locale": self.locale,
            "mode": mode,
            "expose": self.expose,
        })
    }
}

/// `__mcext/shim.js` for pages and the worker, with the instance's token.
pub fn page_shim(config: &ShimConfig, token: &str) -> String {
    let mut cfg = config.base("page");
    cfg["token"] = token.into();
    format!("globalThis.__MCEXT_CFG = {cfg};\n{SHIM}")
}

/// `__mcext/background-host.js`: starts the worker, and says when it fails.
pub fn background_host_script(manifest: &Manifest) -> String {
    let options = if manifest.background_module {
        "{ type: 'module', name: 'background' }"
    } else {
        "{ name: 'background' }"
    };
    format!(
        "globalThis.__mcextWorker = new Worker('/__mcext/background-worker.js', {options});\n\
         __mcextWorker.onerror = (e) => console.error('background worker failed:', e.message, e.filename + ':' + e.lineno);\n"
    )
}

/// `__mcext/background-worker.js`: the shim, then the extension's worker.
pub fn worker_script(manifest: &Manifest) -> String {
    let worker = manifest.background.as_deref().unwrap_or("background.js");
    let worker = Value::String(format!("/{worker}"));
    if manifest.background_module {
        format!("import '/__mcext/shim.js';\nimport {worker};\n")
    } else {
        format!("importScripts('/__mcext/shim.js');\nimportScripts({worker});\n")
    }
}

/// Names a page's programs use, fixed for the app's run: the binding the
/// isolated world calls (`__monocode…`: the external CDP endpoint never
/// shows the app's bindings, K57), the function the app answers through,
/// and the DOM event channel of the externally connectable relay.
pub struct Names {
    pub binding: String,
    pub recv: String,
    pub channel: String,
    pub registry: String,
    pub world: String,
    pub guard: String,
}

impl Names {
    pub fn new(id: &str, secret: &str) -> Self {
        Names {
            // One per extension: a session keeps one world per binding name.
            binding: format!("__monocodeExt_{}", &id[..id.len().min(12)]),
            recv: format!("__monocodeExtRecv_{secret}"),
            channel: format!("mcext-{id}-{secret}"),
            registry: format!("__mcextExternal_{secret}"),
            world: format!("Monocode extension {id}"),
            guard: format!("__mcextMain_{id}_{secret}"),
        }
    }
}

fn regexes(set: &super::pattern::PatternSet) -> Value {
    Value::Array(
        set.0
            .iter()
            .map(|p| Value::String(p.regex_source()))
            .collect(),
    )
}

fn entry_json(script: &ContentScript, dir: &Path) -> Value {
    let read = |file: &String| std::fs::read_to_string(dir.join(file)).unwrap_or_default();
    json!({
        "matches": regexes(&script.matches),
        "exclude": regexes(&script.exclude_matches),
        "include_globs": script.include_globs.iter().map(|g| super::pattern::glob_regex_source(g)).collect::<Vec<_>>(),
        "exclude_globs": script.exclude_globs.iter().map(|g| super::pattern::glob_regex_source(g)).collect::<Vec<_>>(),
        "all_frames": script.all_frames,
        "about_blank": script.about_blank,
        "run_at": script.run_at.as_str(),
        "css": script.css.iter().map(read).collect::<Vec<_>>().join("\n"),
        "names": script.js.clone(),
    })
}

/// What both programs share: which entries a document wants (`WANTED`),
/// and how they run by `run_at` (`SCHEDULE`), over `ENTRIES` and `RUN`.
const HELPERS: &str = r#"
  const href = () => String(location.href);
  const test = (list, url) => list.some((r) => new RegExp(r, "i").test(url));
  // about:blank/srcdoc frames go by their parent's URL when asked.
  const frameUrl = (e) => {
    const u = href();
    if (/^(about|data|blob):/.test(u) && e.about_blank) {
      try { return String(window.parent.location.href); } catch (_) { return u; }
    }
    return u;
  };
  const wants = (e) => {
    if (!e.all_frames && window.top !== window) return false;
    const u = frameUrl(e);
    return test(e.matches, u) && !test(e.exclude, u)
      && (!e.include_globs.length || test(e.include_globs, u)) && !test(e.exclude_globs, u);
  };
  const WANTED = ENTRIES.map(wants);
"#;

const SCHEDULE: &str = r#"
  const css = (text) => {
    if (!text) return;
    const add = () => {
      const s = document.createElement("style");
      s.textContent = text;
      (document.head || document.documentElement).appendChild(s);
    };
    if (document.documentElement) add(); else document.addEventListener("readystatechange", add, { once: true });
  };
  const at = (when, run) => {
    if (when === "document_start") return run();
    if (when === "document_end") {
      if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", run, { once: true });
      else run();
      return;
    }
    if (document.readyState === "complete") return run();
    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => setTimeout(run, 0), { once: true });
    else setTimeout(run, 0);
  };
  ENTRIES.forEach((e, i) => {
    if (!WANTED[i]) return;
    css(e.css);
    at(e.run_at, () => RUN[i]());
  });
"#;

fn program(entries: &[(&ContentScript, Value)], dir: &Path) -> (String, String) {
    let list = Value::Array(entries.iter().map(|(_, e)| e.clone()).collect());
    let mut runs = String::from("[");
    for (script, _) in entries {
        runs.push_str("() => {\n");
        for file in &script.js {
            let source = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            let name = Value::String(file.clone());
            runs.push_str(&format!(
                "try {{ (function () {{\n{source}\n}}).call(globalThis); }} catch (e) {{ console.error('content script ' + {name} + ' failed:', e); }}\n"
            ));
        }
        runs.push_str("},\n");
    }
    runs.push(']');
    (
        format!("const ENTRIES = {list};"),
        format!("const RUN = {runs};"),
    )
}

/// The programs a web page runs for an extension: `(isolated, main)`.
/// `None` where the extension has nothing for that world.
pub fn content_programs(
    config: &ShimConfig,
    dir: &Path,
    names: &Names,
) -> (Option<String>, Option<String>) {
    let manifest = config.manifest;
    let isolated: Vec<(&ContentScript, Value)> = (manifest.content_scripts.iter())
        .filter(|c| c.world == World::Isolated && (!c.js.is_empty() || !c.css.is_empty()))
        .map(|c| (c, entry_json(c, dir)))
        .collect();
    let main: Vec<(&ContentScript, Value)> = (manifest.content_scripts.iter())
        .filter(|c| c.world == World::Main && !c.js.is_empty())
        .map(|c| (c, entry_json(c, dir)))
        .collect();
    let external = (manifest.externally_connectable.as_ref())
        .filter(|e| !e.matches.is_empty())
        .map(|e| regexes(&e.matches));

    let web = "if (!/^(https?|file|about|data|blob):$/.test(location.protocol)) return;";
    let binding = Value::String(names.binding.clone());
    let isolated_program = (!isolated.is_empty() || external.is_some()).then(|| {
        let (entries, runs) = program(&isolated, dir);
        // Once per document; the binding leaves the world's global at once.
        let mut out = format!(
            "(() => {{ {web}\n\
             if (Object.getOwnPropertyDescriptor(globalThis, '__mcextProgram')) return;\n\
             Object.defineProperty(globalThis, '__mcextProgram', {{ value: true }});\n\
             const bind = globalThis[{binding}]; try {{ delete globalThis[{binding}]; }} catch (_) {{}}\n\
             if (typeof bind !== 'function') return;\n"
        );
        if !isolated.is_empty() {
            let mut cfg = config.base("content");
            cfg["recv"] = names.recv.clone().into();
            // The shim only where a script runs.
            out.push_str(&format!(
                "{{ {entries}\n{runs}\n{HELPERS}\n\
                 if (WANTED.some(Boolean)) {{ globalThis.__MCEXT_CFG = {cfg}; globalThis.__MCEXT_CFG.send = bind;\n{SHIM}\n{SCHEDULE} }} }}\n"
            ));
        }
        if let Some(matches) = &external {
            // The relay for the main world's chrome.runtime, where it matches.
            let mut ext = config.base("external");
            ext["recv"] = format!("{}_ext", names.recv).into();
            ext["channel"] = names.channel.clone().into();
            out.push_str(&format!(
                "if (window.top === window && {matches}.some((r) => new RegExp(r, 'i').test(String(location.href)))) {{\n\
                   globalThis.__MCEXT_CFG = {ext}; globalThis.__MCEXT_CFG.send = bind;\n{SHIM}\n\
                 }}\n"
            ));
        }
        out.push_str("})();\n");
        out
    });
    let main_program = (!main.is_empty() || external.is_some()).then(|| {
        let guard = Value::String(names.guard.clone());
        let (entries, runs) = program(&main, dir);
        let mut out = format!(
            "(() => {{ {web}\nif (Object.getOwnPropertyDescriptor(window, {guard})) return; Object.defineProperty(window, {guard}, {{ value: true }});\n"
        );
        if let Some(matches) = &external {
            let ext = json!({"id": config.id, "channel": names.channel, "registry": names.registry});
            out.push_str(&format!(
                "if (window.top === window && {matches}.some((r) => new RegExp(r, 'i').test(String(location.href)))) {{ globalThis.__MCEXT_EXT = {ext};\n{EXTERNAL}\n}}\n"
            ));
        }
        if !main.is_empty() {
            out.push_str(&format!("{{ {entries}\n{runs}\n{HELPERS}\n{SCHEDULE} }}\n"));
        }
        out.push_str("})();\n");
        out
    });
    (isolated_program, main_program)
}

/// Whether the extension has anything for web pages at all.
pub fn touches_pages(manifest: &Manifest) -> bool {
    !manifest.content_scripts.is_empty()
        || manifest
            .externally_connectable
            .as_ref()
            .is_some_and(|e| !e.matches.is_empty())
}

/// `run_at` names, for tests.
pub fn run_at_name(r: RunAt) -> &'static str {
    r.as_str()
}
