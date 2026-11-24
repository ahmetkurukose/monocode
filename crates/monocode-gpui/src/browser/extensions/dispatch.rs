//! The `chrome.*` calls an extension makes, answered on the main thread
//! over the app's own model (K69). Each call is checked against the
//! extension's permissions (`api::required_permission`) and the calling
//! context (content scripts and externally connectable pages reach only
//! part of the API, as in Chrome). Errors use Chrome's texts.

use std::time::Duration;

use monocode_core::browser::extensions::{self as core, api, storage::Area};
use serde_json::{Value, json};

use super::{
    InstanceKey,
    instance::{self, Alarm, Kind},
    pages, tabs, with,
};

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Null)
}

/// What the caller is.
struct Caller {
    kind: Kind,
    tab: Option<i64>,
    url: String,
}

fn caller(key: &InstanceKey, ctx: &str) -> Option<Caller> {
    with(|rt| {
        let c = rt.instances.get(key)?.ctxs.get(ctx)?;
        Some(Caller {
            kind: c.kind,
            tab: c.tab,
            url: c.url.clone(),
        })
    })
    .flatten()
}

/// Calls a content script may make (Chrome's content script API).
fn content_allowed(api: &str) -> bool {
    api.starts_with("ctx.")
        || api.starts_with("port.")
        || api.starts_with("storage.")
        || matches!(
            api,
            "runtime.sendMessage" | "runtime.connect" | "runtime.respond"
        )
}

pub(crate) async fn call(
    key: &InstanceKey,
    ctx: &str,
    api_name: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    if super::dev::dir().is_none() {
        return answer(key, ctx, api_name, args).await;
    }
    // Development hook: the instance's recent calls, for state.json.
    let kind = caller(key, ctx).map(|c| c.kind.as_str()).unwrap_or("?");
    let shown = args
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let result = answer(key, ctx, api_name, args).await;
    let line = format!(
        "{kind} {api_name}({}) -> {}",
        shown.chars().take(160).collect::<String>(),
        match &result {
            Ok(v) => v.to_string().chars().take(160).collect::<String>(),
            Err(e) => format!("ERROR {e}"),
        }
    );
    with(|rt| {
        if let Some(i) = rt.instances.get_mut(key) {
            i.calls.push_back(line);
            while i.calls.len() > 300 {
                i.calls.pop_front();
            }
        }
    });
    result
}

async fn answer(
    key: &InstanceKey,
    ctx: &str,
    api_name: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    let Some(ext) = with(|rt| rt.instances.get(key).map(|i| i.ext.clone())).flatten() else {
        return Err("Extension context invalidated.".into());
    };
    let Some(caller) = caller(key, ctx) else {
        return Err("Extension context invalidated.".into());
    };
    if matches!(caller.kind, Kind::Content | Kind::External) && !content_allowed(api_name) {
        return Err(format!("{api_name} is not available in content scripts."));
    }
    if let Some(permission) = api::required_permission(api_name) {
        let granted = ext.has(permission)
            || (permission == "privacy"
                && with(|rt| {
                    rt.instances
                        .get(key)
                        .is_some_and(|i| i.privacy.contains_key("__granted"))
                }) == Some(true));
        if !granted {
            return Err(api::missing_permission(api_name, permission));
        }
    }
    match api_name {
        // ---- contexts ----
        "ctx.hello" => Ok(with(|rt| {
            let i = rt.instances.get_mut(key)?;
            let kind = i.ctxs.get(ctx)?.kind;
            let mut info = json!({"kind": kind.as_str()});
            if kind == Kind::Background
                && let Some(lifecycle) = i.lifecycle.take()
            {
                for (k, v) in lifecycle.as_object().into_iter().flatten() {
                    info[k] = v.clone();
                }
            }
            Some(info)
        })
        .flatten()
        .unwrap_or_default()),
        "ctx.listen" => {
            let name = arg(&args, 0).as_str().unwrap_or("").to_owned();
            with(|rt| {
                if let Some(c) = rt.instances.get_mut(key).and_then(|i| i.ctxs.get_mut(ctx)) {
                    c.listening.insert(name);
                }
            });
            Ok(Value::Null)
        }
        "ctx.bye" => {
            instance::forget_ctx(key, ctx);
            Ok(Value::Null)
        }
        "popup.close" => {
            if caller.kind == Kind::Popup {
                super::toolbar::close_popup(key);
            }
            Ok(Value::Null)
        }

        // ---- messaging ----
        "runtime.sendMessage" | "runtime.connect" => {
            let target = arg(&args, 0);
            let ours = target.is_null() || target.as_str() == Some(key.id.as_str());
            if !ours {
                return Err(api::NO_RECEIVER.into());
            }
            let external = caller.kind == Kind::External;
            if external {
                let allowed = (ext.manifest.externally_connectable.as_ref())
                    .is_some_and(|e| e.matches.matches(&caller.url));
                if !allowed {
                    return Err(api::NO_RECEIVER.into());
                }
            }
            let connect = api_name == "runtime.connect";
            let event = match (connect, external) {
                (false, false) => "runtime.onMessage",
                (false, true) => "runtime.onMessageExternal",
                (true, false) => "runtime.onConnect",
                (true, true) => "runtime.onConnectExternal",
            };
            let pick = || {
                with(|rt| {
                    let i = rt.instances.get(key)?;
                    Some(
                        i.listeners(event, ctx)
                            .into_iter()
                            .filter(|t| i.ctxs.get(t).is_some_and(|c| c.kind.is_extension_page()))
                            .collect::<Vec<_>>(),
                    )
                })
                .flatten()
                .unwrap_or_default()
            };
            let targets = instance::wait_for(pick).await;
            if connect {
                instance::connect(
                    key,
                    ctx,
                    targets,
                    arg(&args, 1).as_str().unwrap_or(""),
                    &arg(&args, 2),
                    external,
                )?;
                Ok(Value::Null)
            } else {
                instance::send_message(key, ctx, targets, arg(&args, 1), external).await
            }
        }
        "tabs.sendMessage" | "tabs.connect" => {
            let tab = arg(&args, 0).as_i64().ok_or("No tab id")?;
            let connect = api_name == "tabs.connect";
            let frame = if connect {
                arg(&args, 1).as_i64()
            } else {
                arg(&args, 2)["frameId"].as_i64()
            };
            let event = if connect {
                "runtime.onConnect"
            } else {
                "runtime.onMessage"
            };
            let targets: Vec<String> = with(|rt| {
                let i = rt.instances.get(key)?;
                Some(
                    i.listeners(event, ctx)
                        .into_iter()
                        .filter(|t| {
                            i.ctxs.get(t).is_some_and(|c| {
                                c.kind == Kind::Content
                                    && c.tab == Some(tab)
                                    && frame.is_none_or(|f| f == c.frame_id)
                            })
                        })
                        .collect(),
                )
            })
            .flatten()
            .unwrap_or_default();
            // Which frames of the tab have a content script, for a message
            // that reaches none of them (or one frame of several).
            if targets.is_empty() || frame.is_some() {
                let frames: Vec<(i64, bool)> = with(|rt| {
                    let i = rt.instances.get(key)?;
                    let listening = i.listeners(event, ctx);
                    Some(
                        (i.ctxs.iter())
                            .filter(|(_, c)| c.kind == Kind::Content && c.tab == Some(tab))
                            .map(|(id, c)| (c.frame_id, listening.contains(id)))
                            .collect(),
                    )
                })
                .flatten()
                .unwrap_or_default();
                let kind = arg(&args, 1)["type"].as_str().unwrap_or("").to_owned();
                log::warn!(
                    "browser: extension {}: {api_name} {kind} to tab {tab} frame {frame:?}: {} receiver(s); content (frame, listening): {frames:?}",
                    key.id,
                    targets.len()
                );
            }
            if connect {
                instance::connect(
                    key,
                    ctx,
                    targets,
                    arg(&args, 3).as_str().unwrap_or(""),
                    &arg(&args, 4),
                    false,
                )?;
                Ok(Value::Null)
            } else {
                instance::send_message(key, ctx, targets, arg(&args, 1), false).await
            }
        }
        "runtime.respond" => {
            instance::respond(
                key,
                arg(&args, 0).as_str().unwrap_or(""),
                ctx,
                arg(&args, 1).as_bool().unwrap_or(false),
                arg(&args, 2),
            );
            Ok(Value::Null)
        }
        "port.post" => {
            let port = arg(&args, 0).as_str().unwrap_or("").to_owned();
            let ends = with(|rt| rt.instances.get(key)?.ports.get(&port).cloned())
                .flatten()
                .unwrap_or_default();
            for end in ends.iter().filter(|e| *e != ctx) {
                instance::deliver(
                    key,
                    end,
                    json!({"t": "port.message", "port": port, "msg": arg(&args, 1)}),
                );
            }
            Ok(Value::Null)
        }
        "port.disconnect" => {
            let port = arg(&args, 0).as_str().unwrap_or("").to_owned();
            let ends = with(|rt| rt.instances.get_mut(key)?.ports.remove(&port))
                .flatten()
                .unwrap_or_default();
            for end in ends.iter().filter(|e| *e != ctx) {
                instance::deliver(key, end, json!({"t": "port.disconnect", "port": port}));
            }
            Ok(Value::Null)
        }

        // ---- storage ----
        "storage.get"
        | "storage.set"
        | "storage.remove"
        | "storage.clear"
        | "storage.getBytesInUse"
        | "storage.getKeys"
        | "storage.setAccessLevel" => storage(key, &caller, api_name, &args),

        // ---- runtime ----
        "runtime.getPlatformInfo" => Ok(json!({
            "os": "mac",
            "arch": if cfg!(target_arch = "aarch64") { "arm64" } else { "x86-64" },
            "nacl_arch": if cfg!(target_arch = "aarch64") { "arm" } else { "x86-64" },
        })),
        "runtime.getContexts" => {
            let filter = arg(&args, 0);
            let wanted: Option<Vec<String>> = filter["contextTypes"].as_array().map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
            Ok(with(|rt| {
                let profile = key.profile.clone();
                let i = rt.instances.get(key)?;
                let list: Vec<Value> = (i.ctxs.iter())
                    .filter_map(|(id, c)| {
                        let kind = c.kind.context_type()?;
                        if wanted.as_ref().is_some_and(|w| !w.iter().any(|k| k == kind)) {
                            return None;
                        }
                        let window = c.tab.and_then(|t| rt.tabs.window_of(&profile, t)).unwrap_or(-1);
                        Some(json!({
                            "contextId": id, "contextType": kind, "documentId": id,
                            "documentOrigin": core::origin(&key.id),
                            // A worker's is its script.
                            "documentUrl": match (c.kind, &i.ext.manifest.background) {
                                (Kind::Background, Some(script)) => format!("{}{script}", core::base_url(&key.id)),
                                _ => c.url.clone(),
                            },
                            "frameId": if c.tab.is_some() { c.frame_id } else { -1 },
                            "incognito": false, "tabId": c.tab.unwrap_or(-1), "windowId": window,
                        }))
                    })
                    .collect();
                Some(Value::Array(list))
            })
            .flatten()
            .unwrap_or_default())
        }
        "runtime.openOptionsPage" => {
            let page = ext
                .manifest
                .options_page
                .clone()
                .ok_or("Could not create an options page.")?;
            instance::open_tab(key, &page, true);
            Ok(Value::Null)
        }
        "runtime.setUninstallURL" => Ok(Value::Null),
        "runtime.requestUpdateCheck" => Ok(json!({"status": "no_update"})),
        "runtime.reload" => {
            let key = key.clone();
            super::spawn(async move {
                instance::stop(&key);
                super::sleep(Duration::from_millis(200)).await;
                instance::ensure(&key);
            });
            Ok(Value::Null)
        }
        "management.getSelf" => Ok(json!({
            "id": key.id, "name": ext.manifest.name, "shortName": ext.manifest.name,
            "description": ext.manifest.description, "version": ext.manifest.version,
            "enabled": true, "mayDisable": true, "installType": "normal", "type": "extension",
            "offlineEnabled": false, "isApp": false, "hostPermissions": ext.manifest.host_permissions,
            "permissions": ext.permissions,
        })),
        "commands.getAll" => Ok(Value::Array(
            (ext.manifest.commands.iter())
                .map(|c| json!({"name": c.name, "description": c.description, "shortcut": ""}))
                .collect(),
        )),

        // ---- tabs and windows ----
        "tabs.query" => {
            tabs::refresh_now();
            let info = arg(&args, 0);
            Ok(with(|rt| {
                let model = rt.tabs.model(&key.profile);
                let window = caller.tab.and_then(|t| model.tab(t).map(|t| t.window_id));
                let reveal = |t: &core::tabs::TabInfo| ext.has("tabs") || ext.hosts.matches(&t.url);
                Value::Array(
                    model
                        .query(&info, window, &reveal)
                        .into_iter()
                        .map(|t| model.tab_json(t, reveal(t)))
                        .collect(),
                )
            })
            .unwrap_or_default())
        }
        "tabs.get" => {
            tabs::refresh_now();
            let id = arg(&args, 0).as_i64().unwrap_or(-1);
            tab_json(key, &ext, id).ok_or_else(|| api::no_tab(id))
        }
        "tabs.getCurrent" => Ok(match (caller.kind, caller.tab) {
            (Kind::Tab, Some(tab)) => tab_json(key, &ext, tab).unwrap_or(Value::Null),
            _ => Value::Null,
        }),
        "tabs.create" | "windows.create" => {
            let props = arg(&args, 0);
            let url = match &props["url"] {
                Value::Array(a) => a.first().and_then(Value::as_str).unwrap_or("").to_owned(),
                v => v.as_str().unwrap_or("").to_owned(),
            };
            let active = props["active"]
                .as_bool()
                .or(props["focused"].as_bool())
                .unwrap_or(true);
            tabs::refresh_now();
            let before: Vec<i64> = with(|rt| {
                rt.tabs
                    .model(&key.profile)
                    .tabs
                    .iter()
                    .map(|t| t.id)
                    .collect()
            })
            .unwrap_or_default();
            instance::open_tab(
                key,
                if url.is_empty() { "about:blank" } else { &url },
                active,
            );
            let mut made = None;
            for _ in 0..60 {
                super::sleep(Duration::from_millis(100)).await;
                tabs::refresh_now();
                made = with(|rt| {
                    (rt.tabs.model(&key.profile).tabs.iter())
                        .map(|t| t.id)
                        .find(|id| !before.contains(id))
                })
                .flatten();
                if made.is_some() {
                    break;
                }
            }
            let tab = made
                .and_then(|id| tab_json(key, &ext, id))
                .unwrap_or_else(|| json!({"id": -1, "url": url, "pendingUrl": url}));
            if api_name == "windows.create" {
                let window = tab["windowId"].as_i64().unwrap_or(-1);
                return Ok(
                    json!({"id": window, "focused": true, "type": "normal", "state": "normal",
                    "incognito": false, "alwaysOnTop": false, "tabs": [tab]}),
                );
            }
            Ok(tab)
        }
        "tabs.update" => {
            let id = match arg(&args, 0).as_i64() {
                Some(id) => id,
                None => active_tab(key, &caller).ok_or("No current tab")?,
            };
            let props = arg(&args, 1);
            let page = with(|rt| rt.tabs.model(&key.profile).tab(id).is_some()).unwrap_or(false);
            if !page {
                return Err(api::no_tab(id));
            }
            if let Some(url) = props["url"].as_str() {
                let url = if url.contains("://") || url.starts_with("about:") {
                    url.to_owned()
                } else {
                    format!("{}{}", core::base_url(&key.id), url.trim_start_matches('/'))
                };
                if url.starts_with("javascript:") {
                    return Err("JavaScript URLs are not allowed in tabs.update.".into());
                }
                if url.starts_with(&core::base_url(&key.id)) {
                    super::serve::expect(&url);
                }
                with_app(move |cx| {
                    crate::shell::browser_extensions::navigate_extension_tab(id, url, cx)
                });
            }
            if props["active"].as_bool() == Some(true)
                || props["highlighted"].as_bool() == Some(true)
            {
                with_app(move |cx| crate::shell::browser_extensions::show_extension_tab(id, cx));
            }
            if let Some(muted) = props["muted"].as_bool() {
                with_app(move |cx| {
                    crate::shell::browser_extensions::mute_extension_tab(id, muted, cx)
                });
            }
            super::sleep(Duration::from_millis(50)).await;
            tabs::refresh_now();
            tab_json(key, &ext, id).ok_or_else(|| api::no_tab(id))
        }
        "tabs.remove" => {
            for id in arg(&args, 0)
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
            {
                with_app(move |cx| crate::shell::browser_extensions::close_extension_tab(id, cx));
            }
            Ok(Value::Null)
        }
        "tabs.reload" | "tabs.goBack" | "tabs.goForward" => {
            let id = match arg(&args, 0).as_i64() {
                Some(id) => id,
                None => active_tab(key, &caller).ok_or("No current tab")?,
            };
            let cdp = pages::cdp_of(id).ok_or_else(|| api::no_tab(id))?;
            let (method, params) = match api_name {
                "tabs.reload" => (
                    "Page.reload",
                    json!({"ignoreCache": arg(&args, 1)["bypassCache"].as_bool().unwrap_or(false)}),
                ),
                "tabs.goBack" => ("Runtime.evaluate", json!({"expression": "history.back()"})),
                _ => (
                    "Runtime.evaluate",
                    json!({"expression": "history.forward()"}),
                ),
            };
            cdp.call(method, params, None)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "tabs.getZoom" => Ok(json!(1.0)),
        "tabs.duplicate" | "tabs.discard" | "tabs.highlight" => Err(api::unsupported(api_name)),
        "windows.get" | "windows.getCurrent" | "windows.getLastFocused" | "windows.getAll" => {
            tabs::refresh_now();
            let opts = if api_name == "windows.get" {
                arg(&args, 1)
            } else {
                arg(&args, 0)
            };
            let populate = opts["populate"].as_bool() == Some(true);
            Ok(with(|rt| {
                let model = rt.tabs.model(&key.profile);
                let reveal = |t: &core::tabs::TabInfo| ext.has("tabs") || ext.hosts.matches(&t.url);
                let populate_fn: Option<&dyn Fn(&core::tabs::TabInfo) -> bool> =
                    populate.then_some(&reveal);
                let current = model
                    .current_window(caller.tab.and_then(|t| model.tab(t).map(|t| t.window_id)));
                let want = match api_name {
                    "windows.get" => arg(&args, 0)
                        .as_i64()
                        .map(|w| if w == -2 { current.unwrap_or(-1) } else { w }),
                    "windows.getCurrent" => current,
                    "windows.getLastFocused" => model.last_focused.or(current),
                    _ => None,
                };
                if api_name == "windows.getAll" {
                    return Ok(Value::Array(
                        model
                            .windows
                            .iter()
                            .map(|w| model.window_json(w, populate_fn))
                            .collect(),
                    ));
                }
                let id = want.unwrap_or(-1);
                (model.windows.iter())
                    .find(|w| w.id == id)
                    .map(|w| model.window_json(w, populate_fn))
                    .ok_or_else(|| api::no_window(id))
            })
            .unwrap_or_else(|| Err("gone".into()))?)
        }
        "windows.update" => {
            let id = arg(&args, 0).as_i64().unwrap_or(-2);
            if arg(&args, 1)["focused"].as_bool() == Some(true) {
                with_app(move |cx| {
                    crate::shell::browser_extensions::focus_extension_window(id, cx)
                });
            }
            Ok(with(|rt| {
                let model = rt.tabs.model(&key.profile);
                let w = model
                    .windows
                    .iter()
                    .find(|w| w.id == id)
                    .or(model.windows.first())?;
                Some(model.window_json(w, None))
            })
            .flatten()
            .unwrap_or_default())
        }
        "windows.remove" => Err(api::unsupported(api_name)),

        // ---- scripting ----
        "scripting.executeScript" | "scripting.insertCSS" | "scripting.removeCSS" => {
            scripting(key, &ext, api_name, arg(&args, 0)).await
        }
        "scripting.getRegisteredContentScripts" => Ok(json!([])),
        "scripting.registerContentScripts"
        | "scripting.updateContentScripts"
        | "scripting.unregisterContentScripts" => Err(api::unsupported(api_name)),

        // ---- webNavigation ----
        "webNavigation.getFrame" | "webNavigation.getAllFrames" => {
            let q = arg(&args, 0);
            let tab = q["tabId"].as_i64().unwrap_or(-1);
            let list: Vec<Value> = pages::frames(tab)
                .into_iter()
                .map(|f| {
                    json!({
                        "frameId": f.id, "parentFrameId": pages::parent_frame(tab, f.id), "url": f.url,
                        "errorOccurred": false, "documentId": pages::document_id(tab, f.id),
                        "documentLifecycle": "active", "processId": -1,
                        "frameType": if f.id == 0 { "outermost_frame" } else { "sub_frame" },
                    })
                })
                .collect();
            if api_name == "webNavigation.getAllFrames" {
                return Ok(if list.is_empty() {
                    Value::Null
                } else {
                    Value::Array(list)
                });
            }
            let want = q["frameId"].as_i64().unwrap_or(0);
            let by_document = q["documentId"].as_str();
            Ok(list
                .into_iter()
                .find(|f| match by_document {
                    Some(d) => f["documentId"] == d,
                    None => f["frameId"] == want,
                })
                .unwrap_or(Value::Null))
        }

        // ---- action ----
        "action.set" | "action.get" | "action.setIcon" => {
            let result = super::toolbar::action_call(key, &ext, api_name, &args);
            super::changed();
            result
        }
        "action.openPopup" => {
            super::toolbar::open_popup(key, None);
            Ok(Value::Null)
        }

        // ---- alarms ----
        "alarms.create" => {
            let name = arg(&args, 0).as_str().unwrap_or("").to_owned();
            let info = arg(&args, 1);
            let now = instance::epoch_ms();
            let period = info["periodInMinutes"].as_f64();
            let when = info["when"]
                .as_f64()
                .or_else(|| info["delayInMinutes"].as_f64().map(|d| now + d * 60_000.))
                .or_else(|| period.map(|p| now + p * 60_000.))
                .ok_or("Alarm needs a time")?;
            with(|rt| {
                let i = rt.instances.get_mut(key)?;
                i.next += 1;
                let generation = i.next;
                i.alarms.insert(
                    name.clone(),
                    Alarm {
                        scheduled: when,
                        period,
                        generation,
                    },
                );
                Some(())
            });
            instance::schedule_alarm(key, &name);
            Ok(Value::Null)
        }
        "alarms.get" | "alarms.getAll" => {
            let name = arg(&args, 0).as_str().map(str::to_owned);
            Ok(with(|rt| {
                let i = rt.instances.get(key)?;
                let all: Vec<Value> = (i.alarms.iter())
                    .filter(|(n, _)| api_name == "alarms.getAll" || Some(n.as_str()) == name.as_deref())
                    .map(|(n, a)| json!({"name": n, "scheduledTime": a.scheduled, "periodInMinutes": a.period}))
                    .collect();
                Some(if api_name == "alarms.getAll" {
                    Value::Array(all)
                } else {
                    all.into_iter().next().unwrap_or(Value::Null)
                })
            })
            .flatten()
            .unwrap_or_default())
        }
        "alarms.clear" | "alarms.clearAll" => {
            let name = arg(&args, 0).as_str().unwrap_or("").to_owned();
            Ok(Value::Bool(
                with(|rt| {
                    let i = rt.instances.get_mut(key)?;
                    Some(if api_name == "alarms.clearAll" {
                        let any = !i.alarms.is_empty();
                        i.alarms.clear();
                        any
                    } else {
                        i.alarms.remove(&name).is_some()
                    })
                })
                .flatten()
                .unwrap_or(false),
            ))
        }

        // ---- offscreen ----
        "offscreen.createDocument" => {
            let url = arg(&args, 0)["url"].as_str().ok_or("No url")?.to_owned();
            let full = format!("{}{}", core::base_url(&key.id), url.trim_start_matches('/'));
            if with(|rt| rt.instances.get(key).is_some_and(|i| i.offscreen.is_some())) == Some(true)
            {
                return Err("Only a single offscreen document may be created.".into());
            }
            // Clipboard: the document may write it without a gesture.
            let host = instance::open_host(key, &full, true)
                .ok_or("Could not create the offscreen document.")?;
            let browser_id = host.browser_id;
            with(|rt| rt.instances.get_mut(key).map(|i| i.offscreen = Some(host)));
            // Resolves once its page runs, as Chrome's.
            for _ in 0..50 {
                let up = with(|rt| {
                    rt.instances
                        .get(key)
                        .is_some_and(|i| i.ctxs.values().any(|c| c.browser_id == Some(browser_id)))
                })
                .unwrap_or(false);
                if up {
                    break;
                }
                super::sleep(Duration::from_millis(100)).await;
            }
            Ok(Value::Null)
        }
        "offscreen.closeDocument" => {
            let host =
                with(|rt| rt.instances.get_mut(key).and_then(|i| i.offscreen.take())).flatten();
            match host {
                Some(host) => {
                    let id = host.browser_id;
                    drop(host);
                    instance::forget_where(key, |c| c.browser_id == Some(id));
                    Ok(Value::Null)
                }
                None => Err("No current offscreen document.".into()),
            }
        }
        "offscreen.hasDocument" => Ok(Value::Bool(
            with(|rt| rt.instances.get(key).is_some_and(|i| i.offscreen.is_some()))
                .unwrap_or(false),
        )),

        // ---- permissions ----
        "permissions.contains" | "permissions.request" | "permissions.remove" => {
            let q = arg(&args, 0);
            let perms: Vec<String> = (q["permissions"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let origins: Vec<String> = (q["origins"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let privacy_granted = with(|rt| {
                rt.instances
                    .get(key)
                    .is_some_and(|i| i.privacy.contains_key("__granted"))
            })
            .unwrap_or(false);
            let has = |p: &String| ext.has(p) || (p == "privacy" && privacy_granted);
            let hosts_ok = |o: &String| {
                ext.manifest
                    .host_permissions
                    .iter()
                    .any(|h| h == o || h == "<all_urls>")
                    || core::pattern::MatchPattern::parse(o).is_some_and(|_| {
                        let probe = o.replace("*.", "x.").replace('*', "x");
                        ext.hosts.matches(&probe)
                    })
            };
            match api_name {
                "permissions.contains" => Ok(Value::Bool(
                    perms.iter().all(has) && origins.iter().all(hosts_ok),
                )),
                "permissions.remove" => Ok(Value::Bool(false)),
                _ => {
                    // Optional permissions the layer honors without a prompt:
                    // `privacy` only stores settings. Others are declined.
                    let optional = &ext.manifest.optional_permissions;
                    let grantable = |p: &String| has(p) || (p == "privacy" && optional.contains(p));
                    if perms.iter().all(grantable) && origins.iter().all(hosts_ok) {
                        if perms.iter().any(|p| p == "privacy") && !privacy_granted {
                            with(|rt| {
                                rt.instances.get_mut(key).map(|i| {
                                    i.privacy.insert("__granted".into(), Value::Bool(true))
                                })
                            });
                            instance::broadcast(
                                key,
                                "permissions.onAdded",
                                json!([{"permissions": ["privacy"], "origins": []}]),
                                false,
                            );
                        }
                        Ok(Value::Bool(true))
                    } else {
                        Ok(Value::Bool(false))
                    }
                }
            }
        }
        "permissions.getAll" => Ok(json!({
            "permissions": ext.permissions,
            "origins": ext.manifest.host_permissions,
        })),

        // ---- privacy (bookkeeping: stored, never enforced) ----
        "privacy.get" | "privacy.set" | "privacy.clear" => {
            let name = arg(&args, 0).as_str().unwrap_or("").to_owned();
            let details = arg(&args, 1);
            Ok(with(|rt| {
                let i = rt.instances.get_mut(key)?;
                match api_name {
                    "privacy.set" => {
                        i.privacy.insert(name, details["value"].clone());
                        Some(Value::Null)
                    }
                    "privacy.clear" => {
                        i.privacy.remove(&name);
                        Some(Value::Null)
                    }
                    _ => {
                        let set = i.privacy.get(&name).cloned();
                        let value = set.clone().unwrap_or(match name.as_str() {
                            "network.webRTCIPHandlingPolicy" => json!("default"),
                            _ => json!(true),
                        });
                        Some(json!({"value": value, "levelOfControl": if set.is_some() { "controlled_by_this_extension" } else { "controllable_by_this_extension" }}))
                    }
                }
            })
            .flatten()
            .unwrap_or_default())
        }

        _ => Err(api::unsupported(api_name)),
    }
}

fn with_app(f: impl FnOnce(&mut gpui::App) + 'static) {
    if let Some(app) = with(|rt| rt.app.clone()) {
        let executor = app.foreground_executor().clone();
        executor.spawn(async move { app.update(f) }).detach();
    }
}

fn tab_json(key: &InstanceKey, ext: &super::Ext, id: i64) -> Option<Value> {
    with(|rt| {
        let model = rt.tabs.model(&key.profile);
        let t = model.tab(id)?;
        Some(model.tab_json(t, ext.has("tabs") || ext.hosts.matches(&t.url)))
    })
    .flatten()
}

fn active_tab(key: &InstanceKey, caller: &Caller) -> Option<i64> {
    if let Some(tab) = caller.tab {
        return Some(tab);
    }
    tabs::refresh_now();
    with(|rt| {
        let model = rt.tabs.model(&key.profile);
        let window = model.current_window(None)?;
        model.active_in(window)
    })
    .flatten()
}

fn storage(
    key: &InstanceKey,
    caller: &Caller,
    api_name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let area_name = arg(args, 0).as_str().unwrap_or("local").to_owned();
    let area = Area::parse(&area_name).ok_or("Invalid storage area")?;
    let untrusted = matches!(caller.kind, Kind::Content | Kind::External);
    let result = with(|rt| {
        let i = rt.instances.get_mut(key)?;
        if area == Area::Session && untrusted && !i.session_untrusted {
            return Some(Err(
                "Access to storage is not allowed from this context.".to_owned()
            ));
        }
        if api_name == "storage.setAccessLevel" {
            if untrusted {
                return Some(Err(
                    "Access to storage is not allowed from this context.".into()
                ));
            }
            i.session_untrusted = arg(args, 1)["accessLevel"] == "TRUSTED_AND_UNTRUSTED_CONTEXTS";
            return Some(Ok((Value::Null, None)));
        }
        let Some(s) = i.storage.as_mut() else {
            return Some(Err("Storage is unavailable.".into()));
        };
        Some(match api_name {
            "storage.get" => s.get(area, &arg(args, 1)).map(|v| (v, None)),
            "storage.getKeys" => s.get(area, &Value::Null).map(|v| {
                (
                    Value::Array(
                        v.as_object()
                            .map(|m| m.keys().map(|k| Value::String(k.clone())).collect())
                            .unwrap_or_default(),
                    ),
                    None,
                )
            }),
            "storage.getBytesInUse" => Ok((json!(s.bytes_in_use(area, &arg(args, 1))), None)),
            "storage.set" => s.set(area, &arg(args, 1)).map(|c| (Value::Null, Some(c))),
            "storage.remove" => s
                .remove(area, &arg(args, 1))
                .map(|c| (Value::Null, Some(c))),
            _ => s.clear(area).map(|c| (Value::Null, Some(c))),
        })
    })
    .flatten()
    .ok_or("Extension context invalidated.")??;
    let (value, changes) = result;
    if let Some(changes) = changes.filter(|c| !c.is_empty()) {
        let changes = Value::Object(changes);
        instance::broadcast(key, "storage.onChanged", json!([changes, area_name]), false);
        instance::broadcast(
            key,
            &format!("storage.{area_name}.onChanged"),
            json!([changes]),
            false,
        );
    }
    Ok(value)
}

/// `scripting.executeScript` / `insertCSS` / `removeCSS` over CDP.
async fn scripting(
    key: &InstanceKey,
    ext: &super::Ext,
    api_name: &str,
    inj: Value,
) -> Result<Value, String> {
    let tab = inj["target"]["tabId"].as_i64().ok_or("No tabId")?;
    let all = inj["target"]["allFrames"].as_bool().unwrap_or(false);
    let wanted: Option<Vec<i64>> = inj["target"]["frameIds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect());
    let frames = pages::frames(tab);
    if frames.is_empty() && pages::cdp_of(tab).is_none() {
        return Err(api::no_tab(tab));
    }
    // Host access to the frame, as Chrome checks.
    let url_of = |f: i64| {
        frames
            .iter()
            .find(|x| x.id == f)
            .map(|x| x.url.clone())
            .unwrap_or_default()
    };
    let targets: Vec<i64> = match &wanted {
        Some(ids) => ids.clone(),
        None if all => frames.iter().map(|f| f.id).collect(),
        None => vec![0],
    };
    let main = inj["world"].as_str() == Some("MAIN");
    let read = |files: &Value| -> Result<String, String> {
        let mut out = String::new();
        for f in files
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let path = ext.dir.join(f.trim_start_matches('/'));
            let text = std::fs::read_to_string(&path)
                .map_err(|_| format!("Could not load file: '{f}'."))?;
            out.push_str(&text);
            out.push_str("\n;\n");
        }
        Ok(out)
    };
    let expression = match api_name {
        "scripting.executeScript" => {
            if let Some(func) = inj["func"].as_str() {
                let args = if inj["args"].is_array() {
                    inj["args"].clone()
                } else {
                    json!([])
                };
                format!("(async () => ({func})(...{args}))()")
            } else {
                read(&inj["files"])?
            }
        }
        _ => {
            let css = match inj["css"].as_str() {
                Some(css) => css.to_owned(),
                None => read(&inj["files"])?,
            };
            let id = format!("mcext-css-{:x}", {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                css.hash(&mut h);
                h.finish()
            });
            if api_name == "scripting.insertCSS" {
                format!(
                    "(() => {{ const s = document.createElement('style'); s.dataset.mcext = {id:?}; s.textContent = {}; (document.head || document.documentElement).appendChild(s); }})()",
                    Value::String(css)
                )
            } else {
                format!(
                    "document.querySelectorAll('style[data-mcext={id:?}]').forEach((s) => s.remove())"
                )
            }
        }
    };
    let cdp = pages::cdp_of(tab).ok_or_else(|| api::no_tab(tab))?;
    let mut results = Vec::new();
    for frame in targets {
        let url = url_of(frame);
        if !url.is_empty() && !ext.hosts.matches(&url) && !ext.has("activeTab") {
            if wanted.is_some() {
                return Err("Cannot access contents of the page. Extension manifest must request permission to access the respective host.".into());
            }
            continue;
        }
        let Some((session, context)) = pages::context_for(tab, frame, &key.id, main).await else {
            if wanted.is_some() {
                return Err(format!("No frame with id {frame} in tab {tab}."));
            }
            continue;
        };
        let mut params = json!({"expression": expression, "awaitPromise": true, "returnByValue": true, "userGesture": true});
        if let Some(context) = context {
            params["contextId"] = context.into();
        }
        let r = cdp
            .call("Runtime.evaluate", params, session.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        if let Some(ex) = r.get("exceptionDetails")
            && api_name == "scripting.executeScript"
        {
            let text = ex["exception"]["description"]
                .as_str()
                .or(ex["text"].as_str())
                .unwrap_or("error");
            results.push(json!({"frameId": frame, "documentId": pages::document_id(tab, frame), "error": text}));
            continue;
        }
        results.push(json!({"frameId": frame, "documentId": pages::document_id(tab, frame), "result": r["result"]["value"]}));
    }
    Ok(if api_name == "scripting.executeScript" {
        Value::Array(results)
    } else {
        Value::Null
    })
}

// ---- development hooks ----

pub(crate) fn dev_out(dir: &std::path::Path, value: Value) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("out.jsonl"))
    {
        let _ = writeln!(f, "{value}");
    }
}

pub(crate) fn dev_command(cmd: Value, dir: std::path::PathBuf) {
    let first_key = |name: &str| cmd[name].as_str().map(str::to_owned);
    let default_key = |id: &str| InstanceKey {
        profile: None,
        id: id.to_owned(),
    };
    if let Some(input) = first_key("install") {
        let r = super::install(&input);
        dev_out(
            &dir,
            json!({"install": input, "started": r.is_ok(), "error": r.err()}),
        );
    } else if let Some(id) = first_key("enable") {
        super::set_enabled(&id, cmd["on"].as_bool().unwrap_or(true));
        dev_out(&dir, json!({"enable": id, "on": cmd["on"]}));
    } else if let Some(id) = first_key("uninstall") {
        super::uninstall(&id);
        dev_out(&dir, json!({"uninstall": id}));
    } else if cmd["updates"].as_bool() == Some(true) {
        super::check_updates(true);
        dev_out(&dir, json!({"updates": "started"}));
    } else if let Some(id) = first_key("popup") {
        super::toolbar::open_popup(&default_key(&id), None);
        dev_out(&dir, json!({"popup": id}));
    } else if let Some(id) = first_key("closePopup") {
        super::toolbar::close_popup(&default_key(&id));
        dev_out(&dir, json!({"closePopup": id}));
    } else if let Some(id) = first_key("crash") {
        let cdp = with(|rt| {
            rt.instances
                .get(&default_key(&id))?
                .background
                .as_ref()?
                .cdp
                .clone()
        })
        .flatten();
        if let Some(cdp) = cdp {
            let call = cdp.call("Page.crash", json!({}), None);
            super::spawn(async move {
                let _ = call.await;
            });
        }
        dev_out(&dir, json!({"crash": id}));
    } else if let Some(expr) = first_key("eval") {
        let id = first_key("id").unwrap_or_else(|| core::PROTON_PASS_ID.into());
        let target = first_key("in").unwrap_or_else(|| "background".into());
        let key = default_key(&id);
        super::spawn(async move {
            let result = dev_eval(&key, &target, &expr).await;
            dev_out(&dir, json!({"eval": expr, "in": target, "result": result}));
        });
    } else if cmd["work"].as_bool() == Some(true) {
        dev_out(
            &dir,
            json!({"work": format!("{:?}", super::work()), "listings": super::listings().iter().map(|l| json!({"id": l.id, "name": l.name, "version": l.version, "enabled": l.enabled, "held": l.held.as_ref().map(|h| h.version.clone()), "warnings": l.warnings})).collect::<Vec<_>>()}),
        );
    } else if cmd["tabs"].as_bool() == Some(true) {
        tabs::refresh_now();
        let model = with(|rt| rt.tabs.dev_json()).unwrap_or_default();
        dev_out(&dir, json!({"tabs": model}));
    } else {
        dev_out(&dir, json!({"unknown": cmd}));
    }
}

/// Evaluates `expr` in the background worker (`in: background`) or the
/// popup, with the extension's `chrome` as `__mcextChrome`-free code: the
/// worker's own global.
async fn dev_eval(key: &InstanceKey, target: &str, expr: &str) -> Value {
    let cdp = with(|rt| {
        let i = rt.instances.get(key)?;
        match target {
            "popup" => i.popup.as_ref().and_then(|p| p.cdp()),
            "offscreen" => i.offscreen.as_ref()?.cdp.clone(),
            _ => i.background.as_ref()?.cdp.clone(),
        }
    })
    .flatten();
    let Some(cdp) = cdp else {
        return json!({"error": "no such host (is MONOCODE_BROWSER_EXT_DEV set?)"});
    };
    let session = if target == "background" {
        match cdp.call("Target.getTargets", json!({}), None).await {
            Ok(_) => {}
            Err(e) => return json!({"error": e.to_string()}),
        }
        // The worker's session, attached at start.
        let attached = cdp
            .call(
                "Target.attachToTarget",
                json!({"targetId": worker_target(&cdp).await, "flatten": true}),
                None,
            )
            .await;
        attached
            .ok()
            .and_then(|v| v["sessionId"].as_str().map(str::to_owned))
    } else {
        None
    };
    match cdp
        .call(
            "Runtime.evaluate",
            json!({"expression": expr, "awaitPromise": true, "returnByValue": true}),
            session.as_deref(),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => json!({"error": e.to_string()}),
    }
}

async fn worker_target(cdp: &monocode_core::browser::CdpClient) -> Value {
    let targets = cdp
        .call("Target.getTargets", json!({}), None)
        .await
        .unwrap_or_default();
    (targets["targetInfos"].as_array().into_iter().flatten())
        .find(|t| t["type"] == "worker")
        .map(|t| t["targetId"].clone())
        .unwrap_or(Value::Null)
}
