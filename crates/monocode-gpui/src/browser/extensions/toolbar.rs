//! Extension action buttons in a page's toolbar, and their popups (K69,
//! K44): the button shows the action's icon, badge and title (per tab
//! over global over the manifest's); a click opens the popup in a window
//! of its own, anchored under the button, sized to its page (Chrome's
//! popups: 25×25 to 800×600), focused, closed on blur, Esc or
//! `window.close()`. A click on the button just after a blur closed the
//! popup leaves it closed, as Chrome.

use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::Engine as _;
use gpui::{
    AnyElement, Bounds, ElementId, Image, ImageFormat, Pixels, Window, canvas, div, img,
    prelude::*, px,
};
use monocode_core::browser::{CdpClient, extensions::api};
use serde_json::{Value, json};

use super::{Ext, InstanceKey, spawn, with};
use crate::theme::content;

/// Chrome's popup size limits.
const MIN: (i32, i32) = (25, 25);
const MAX: (i32, i32) = (800, 600);
/// A click this soon after a blur closed the popup is that same close.
const REOPEN_GRACE: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq)]
enum Icon {
    /// A file of the extension.
    Path(String),
    Png(Arc<Image>),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Props {
    title: Option<String>,
    badge_text: Option<String>,
    badge_background: Option<[u8; 4]>,
    badge_text_color: Option<[u8; 4]>,
    popup: Option<String>,
    enabled: Option<bool>,
    icon: Option<Icon>,
}

impl Props {
    fn over(&self, base: &Props) -> Props {
        Props {
            title: self.title.clone().or(base.title.clone()),
            badge_text: self.badge_text.clone().or(base.badge_text.clone()),
            badge_background: self.badge_background.or(base.badge_background),
            badge_text_color: self.badge_text_color.or(base.badge_text_color),
            popup: self.popup.clone().or(base.popup.clone()),
            enabled: self.enabled.or(base.enabled),
            icon: self.icon.clone().or(base.icon.clone()),
        }
    }
}

/// An extension's action state: global, and per tab.
#[derive(Default)]
pub(crate) struct ActionState {
    global: Props,
    tabs: HashMap<i64, Props>,
}

impl ActionState {
    fn for_tab(&self, tab: Option<i64>) -> Props {
        match tab.and_then(|t| self.tabs.get(&t)) {
            Some(p) => p.over(&self.global),
            None => self.global.clone(),
        }
    }

    pub fn dev_json(&self) -> Value {
        let one = |p: &Props| {
            json!({"title": p.title, "badge": p.badge_text, "popup": p.popup, "enabled": p.enabled,
                   "icon": match &p.icon { Some(Icon::Path(s)) => json!(s), Some(Icon::Png(_)) => json!("imageData"), None => Value::Null }})
        };
        json!({"global": one(&self.global), "tabs": self.tabs.iter().map(|(t, p)| (t.to_string(), one(p))).collect::<serde_json::Map<_, _>>()})
    }
}

fn color(v: &Value) -> Option<[u8; 4]> {
    match v {
        Value::Array(a) if a.len() >= 3 => {
            let c = |i: usize| a.get(i).and_then(Value::as_u64).unwrap_or(255).min(255) as u8;
            Some([c(0), c(1), c(2), c(3)])
        }
        Value::String(s) => {
            let hex = s.trim_start_matches('#');
            let hex = if hex.len() == 3 {
                hex.chars().flat_map(|c| [c, c]).collect()
            } else {
                hex.to_owned()
            };
            let n = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
            Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255])
        }
        _ => None,
    }
}

/// `action.set` / `action.get` / `action.setIcon`.
pub(crate) fn action_call(
    key: &InstanceKey,
    ext: &Ext,
    api_name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let manifest_title = (ext.manifest.action.as_ref())
        .and_then(|a| a.title.clone())
        .unwrap_or_else(|| ext.manifest.name.clone());
    let manifest_popup = ext.manifest.action.as_ref().and_then(|a| a.popup.clone());
    with(|rt| {
        let i = rt.instances.get_mut(key).ok_or("gone")?;
        if api_name == "action.setIcon" {
            let details = args.first().cloned().unwrap_or_default();
            let tab = details["tabId"].as_i64();
            let icon = if let Some(data) = details["imageData"].as_object() {
                let best = data
                    .values()
                    .filter(|v| !v.is_null())
                    .max_by_key(|v| v["width"].as_u64().unwrap_or(0));
                let Some(best) = best else {
                    return Err("Invalid imageData".into());
                };
                let rgba = base64::engine::general_purpose::STANDARD
                    .decode(best["rgba"].as_str().unwrap_or(""))
                    .map_err(|_| "Invalid imageData".to_string())?;
                let w = best["width"].as_u64().unwrap_or(0) as u32;
                let h = best["height"].as_u64().unwrap_or(0) as u32;
                let png = api::icon_png(w, h, &rgba).ok_or("Invalid imageData")?;
                Icon::Png(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
            } else {
                let path = match &details["path"] {
                    Value::String(s) => s.clone(),
                    Value::Object(m) => {
                        let mut sizes: Vec<(u32, String)> = (m.iter())
                            .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_owned())))
                            .collect();
                        sizes.sort();
                        (sizes.iter().find(|(s, _)| *s >= 32).or(sizes.last()))
                            .map(|(_, p)| p.clone())
                            .ok_or("Invalid path")?
                    }
                    _ => return Err("Either path or imageData must be specified.".into()),
                };
                Icon::Path(path.trim_start_matches('/').to_owned())
            };
            let props = match tab {
                Some(t) => i.action.tabs.entry(t).or_default(),
                None => &mut i.action.global,
            };
            props.icon = Some(icon);
            return Ok(Value::Null);
        }
        let prop = args
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let details = args.get(1).cloned().unwrap_or_default();
        let tab = details["tabId"].as_i64();
        if api_name == "action.get" {
            let p = i.action.for_tab(tab);
            return Ok(match prop.as_str() {
                "badgeText" => json!(p.badge_text.unwrap_or_default()),
                "badgeBackgroundColor" => json!(p.badge_background.unwrap_or([96, 96, 96, 255])),
                "badgeTextColor" => json!(p.badge_text_color.unwrap_or([255, 255, 255, 255])),
                "title" => json!(p.title.unwrap_or(manifest_title.clone())),
                "popup" => json!(
                    p.popup
                        .or(manifest_popup.clone())
                        .map(|p| if p.contains("://") || p.is_empty() {
                            p
                        } else {
                            format!(
                                "{}{p}",
                                monocode_core::browser::extensions::base_url(&key.id)
                            )
                        })
                        .unwrap_or_default()
                ),
                "enabled" => json!(p.enabled.unwrap_or(true)),
                _ => Value::Null,
            });
        }
        let props = match tab {
            Some(t) => i.action.tabs.entry(t).or_default(),
            None => &mut i.action.global,
        };
        match prop.as_str() {
            "badgeText" => props.badge_text = details["text"].as_str().map(str::to_owned),
            "badgeBackgroundColor" => props.badge_background = color(&details["color"]),
            "badgeTextColor" => props.badge_text_color = color(&details["color"]),
            "title" => props.title = details["title"].as_str().map(str::to_owned),
            "popup" => {
                props.popup = details["popup"].as_str().map(|p| {
                    let base = monocode_core::browser::extensions::base_url(&key.id);
                    p.strip_prefix(&base)
                        .unwrap_or(p)
                        .trim_start_matches('/')
                        .to_owned()
                })
            }
            "enabled" => props.enabled = details["value"].as_bool(),
            _ => return Err(api::unsupported(&format!("action.{prop}"))),
        }
        Ok(Value::Null)
    })
    .unwrap_or_else(|| Err("gone".into()))
}

// ---- the popup ----

pub(crate) struct Popup {
    window: monocode_cef::ExtWindow,
    view: Rc<monocode_cef::BrowserView>,
    browser_id: i32,
    was_key: Cell<bool>,
    cdp: Option<Rc<CdpClient>>,
    _attachment: Option<Box<dyn std::any::Any>>,
}

impl Popup {
    pub fn browser_id(&self) -> i32 {
        self.browser_id
    }

    pub fn cdp(&self) -> Option<Rc<CdpClient>> {
        self.cdp.clone()
    }

    pub fn close(mut self) {
        super::serve::remove_host(self.browser_id);
        self.view.close();
        self.window.close();
    }
}

/// Where a button was last drawn, per extension: the window's view and
/// the button's bounds in it (points, top-left).
#[derive(Clone, Copy)]
struct Placed {
    view: *mut std::ffi::c_void,
    right: f64,
    bottom: f64,
}

thread_local! {
    static PLACED: std::cell::RefCell<HashMap<String, Placed>> = std::cell::RefCell::new(HashMap::new());
    static CLOSED_AT: std::cell::RefCell<HashMap<String, Instant>> = std::cell::RefCell::new(HashMap::new());
}

pub(crate) fn close_popup(key: &InstanceKey) {
    let popup = with(|rt| rt.instances.get_mut(key)?.popup.take()).flatten();
    if let Some(popup) = popup {
        let id = popup.browser_id;
        popup.close();
        super::instance::forget_where(key, |c| c.browser_id == Some(id));
        CLOSED_AT.with(|c| c.borrow_mut().insert(key.id.clone(), Instant::now()));
        super::changed();
    }
}

/// The button's click (or `action.openPopup`): opens the popup, or closes
/// it if open. Without a popup page, `action.onClicked` fires instead.
pub(crate) fn open_popup(key: &InstanceKey, tab: Option<i64>) {
    if !super::instance::ensure(key) {
        return;
    }
    if with(|rt| rt.instances.get(key).is_some_and(|i| i.popup.is_some())) == Some(true) {
        close_popup(key);
        return;
    }
    let just_closed = CLOSED_AT.with(|c| {
        c.borrow()
            .get(&key.id)
            .is_some_and(|t| t.elapsed() < REOPEN_GRACE)
    });
    if just_closed {
        return;
    }
    let Some((popup, ext)) = with(|rt| {
        let i = rt.instances.get(key)?;
        let p = i.action.for_tab(tab);
        let popup = p
            .popup
            .or(i.ext.manifest.action.as_ref().and_then(|a| a.popup.clone()));
        Some((popup, i.ext.clone()))
    })
    .flatten() else {
        return;
    };
    let Some(popup) = popup.filter(|p| !p.is_empty()) else {
        let tab_json = tab.and_then(|t| {
            with(|rt| {
                rt.tabs
                    .tab_json(&key.profile, t, ext.has("tabs") || ext.has("activeTab"))
            })
            .flatten()
        });
        super::instance::broadcast(key, "action.onClicked", json!([tab_json]), false);
        return;
    };
    let url = format!(
        "{}{}",
        monocode_core::browser::extensions::base_url(&key.id),
        popup.trim_start_matches('/')
    );
    // Under its button as last drawn; else under the main window's corner.
    let placed = PLACED
        .with(|p| p.borrow().get(&key.id).copied())
        .unwrap_or(Placed {
            view: std::ptr::null_mut(),
            right: 0.,
            bottom: 0.,
        });
    let (w, h) = (360.0, 480.0);
    let Some(mut window) = monocode_cef::ExtWindow::popup(w, h) else {
        return;
    };
    super::serve::expect(&url);
    let options = monocode_cef::CreateOptions {
        profile: super::profile_of(&key.profile),
        clipboard: false,
    };
    let frame = monocode_cef::Frame {
        x: 0.,
        y: 0.,
        width: w,
        height: h,
    };
    let (view, events) = match monocode_cef::BrowserView::create_with(
        window.content_view(),
        frame,
        &url,
        &options,
    ) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("browser: extension {}: popup: {e}", key.id);
            return;
        }
    };
    let view = Rc::new(view);
    let browser_id = view.browser_id();
    super::serve::add_host(browser_id, &key.id);
    view.set_auto_resize(MIN, MAX);
    window.show(&monocode_cef::PopupAnchor {
        view: placed.view,
        right: placed.right,
        bottom: placed.bottom,
    });
    view.set_focus(true);
    let (cdp, attachment) = match super::dev::dir() {
        Some(_) => dev_attach(&view),
        None => (None, None),
    };
    with(|rt| {
        if let Some(i) = rt.instances.get_mut(key) {
            i.popup = Some(Popup {
                window,
                view: view.clone(),
                browser_id,
                was_key: Cell::new(false),
                cdp,
                _attachment: attachment,
            });
        }
    });
    super::changed();
    // The page's events: its size, its own close.
    let k = key.clone();
    let weak = Rc::downgrade(&view);
    spawn(async move {
        while let Ok(event) = events.recv().await {
            use monocode_cef::BrowserEvent as E;
            match event {
                E::AutoResize { width, height } => {
                    let (w, h) = (
                        width.clamp(MIN.0, MAX.0) as f64,
                        height.clamp(MIN.1, MAX.1) as f64,
                    );
                    with(|rt| {
                        if let Some(p) = rt.instances.get(&k).and_then(|i| i.popup.as_ref())
                            && p.browser_id == browser_id
                        {
                            p.window.set_size(w, h);
                        }
                    });
                    if let Some(view) = weak.upgrade() {
                        view.set_frame(monocode_cef::Frame {
                            x: 0.,
                            y: 0.,
                            width: w,
                            height: h,
                        });
                    }
                }
                E::CloseRequested | E::RenderProcessGone => close_popup(&k),
                E::PopupRequested { url, .. } => {
                    super::instance::open_tab(&k, &url, true);
                }
                E::PopupOpened { id, url, .. } => {
                    drop(monocode_cef::take_popup(id));
                    super::instance::open_tab(&k, &url, true);
                }
                E::Closed => break,
                _ => {}
            }
        }
    });
    // Closes on blur, as Chrome's.
    let k = key.clone();
    spawn(async move {
        loop {
            super::sleep(Duration::from_millis(120)).await;
            let state = with(|rt| {
                let p = rt.instances.get(&k)?.popup.as_ref()?;
                if p.browser_id != browser_id {
                    return None;
                }
                let key_now = p.window.is_key();
                if key_now {
                    p.was_key.set(true);
                }
                Some(p.was_key.get() && !key_now)
            })
            .flatten();
            match state {
                None => break,
                Some(true) => {
                    close_popup(&k);
                    break;
                }
                Some(false) => {}
            }
        }
    });
}

fn dev_attach(
    view: &Rc<monocode_cef::BrowserView>,
) -> (Option<Rc<CdpClient>>, Option<Box<dyn std::any::Any>>) {
    let weak = Rc::downgrade(view);
    let cdp = CdpClient::new(Box::new(move |m: &[u8]| {
        weak.upgrade().is_some_and(|v| v.devtools_send(m))
    }));
    let sink = Rc::downgrade(&cdp);
    let attachment = view.devtools_attach(Box::new(move |m| {
        if let Some(cdp) = sink.upgrade() {
            cdp.on_message(m);
        }
    }));
    match attachment {
        Some(a) => (Some(cdp), Some(Box::new(a))),
        None => (None, None),
    }
}

// ---- the buttons ----

/// The action buttons for a page of `profile` (its tab `tab`).
pub(crate) fn buttons(profile: &monocode_cef::Profile, tab: i64) -> Vec<AnyElement> {
    let Some(profile) = super::profile_key(profile) else {
        return Vec::new();
    };
    let list: Vec<(InstanceKey, Arc<Ext>, Props, bool)> = with(|rt| {
        let mut exts: Vec<Arc<Ext>> = rt
            .exts
            .values()
            .filter(|e| e.manifest.action.is_some())
            .cloned()
            .collect();
        exts.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        exts.into_iter()
            .map(|ext| {
                let key = InstanceKey {
                    profile: profile.clone(),
                    id: ext.id.clone(),
                };
                let (props, open) = match rt.instances.get(&key) {
                    Some(i) => (i.action.for_tab(Some(tab)), i.popup.is_some()),
                    None => (Props::default(), false),
                };
                (key, ext, props, open)
            })
            .collect()
    })
    .unwrap_or_default();
    list.into_iter()
        .enumerate()
        .map(|(n, (key, ext, props, open))| button(n, key, ext, props, open, tab))
        .collect()
}

fn rgba(c: [u8; 4]) -> gpui::Rgba {
    gpui::Rgba {
        r: f32::from(c[0]) / 255.,
        g: f32::from(c[1]) / 255.,
        b: f32::from(c[2]) / 255.,
        a: f32::from(c[3]) / 255.,
    }
}

fn button(
    n: usize,
    key: InstanceKey,
    ext: Arc<Ext>,
    props: Props,
    open: bool,
    tab: i64,
) -> AnyElement {
    let title = props
        .title
        .clone()
        .or_else(|| ext.manifest.action.as_ref().and_then(|a| a.title.clone()))
        .unwrap_or_else(|| ext.manifest.name.clone());
    let enabled = props.enabled.unwrap_or(true);
    let icon: AnyElement = match &props.icon {
        Some(Icon::Png(image)) => img(image.clone()).size(px(16.)).into_any_element(),
        Some(Icon::Path(p)) => img(ext.dir.join(p)).size(px(16.)).into_any_element(),
        None => match ext.manifest.icon(32) {
            Some(p) => img(ext.dir.join(p)).size(px(16.)).into_any_element(),
            None => div()
                .size(px(16.))
                .rounded_sm()
                .bg(content(0.3))
                .into_any_element(),
        },
    };
    let badge = props
        .badge_text
        .clone()
        .filter(|t| !t.is_empty())
        .map(|text| {
            let bg = props.badge_background.unwrap_or([96, 96, 96, 255]);
            let fg = props.badge_text_color.unwrap_or([255, 255, 255, 255]);
            div()
                .absolute()
                .bottom(px(-1.))
                .right(px(-3.))
                .px(px(2.))
                .h(px(10.))
                .min_w(px(10.))
                .rounded(px(3.))
                .bg(rgba(bg))
                .text_color(rgba(fg))
                .text_size(px(8.))
                .line_height(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .child(text.chars().take(4).collect::<String>())
        });
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
    let record = bounds.clone();
    let id = key.id.clone();
    div()
        .id(ElementId::NamedInteger(
            "browser-extension".into(),
            n as u64,
        ))
        .role(gpui::Role::Button)
        .aria_label(gpui::SharedString::from(title.clone()))
        .relative()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded_md()
        .cursor_pointer()
        .when(!enabled, |d| d.opacity(0.45))
        .when(open, |d| d.bg(content(0.12)))
        .hover(|s| s.bg(content(0.1)))
        .child(icon)
        .children(badge)
        .child(
            canvas(
                move |b, _, _| {
                    record.set(Some(b));
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .on_click(move |_, window, _| {
            let zoom = f64::from(window.content_zoom());
            if let Some(b) = bounds.get()
                && let Some(view) = window_view(window)
            {
                let placed = Placed {
                    view,
                    right: f64::from(f32::from(b.right())) * zoom,
                    bottom: f64::from(f32::from(b.bottom())) * zoom + 4.,
                };
                PLACED.with(|p| p.borrow_mut().insert(id.clone(), placed));
            }
            open_popup(&key, Some(tab));
        })
        .into_any_element()
}

/// The window's own AppKit view.
fn window_view(window: &Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_layered_state() {
        assert_eq!(color(&json!("#ff8000")), Some([255, 128, 0, 255]));
        assert_eq!(color(&json!("#f80")), Some([255, 136, 0, 255]));
        assert_eq!(color(&json!([1, 2, 3, 4])), Some([1, 2, 3, 4]));
        assert_eq!(color(&json!("nope")), None);
        let mut s = ActionState::default();
        s.global.badge_text = Some("1".into());
        s.global.title = Some("Global".into());
        s.tabs.insert(
            7,
            Props {
                badge_text: Some("9".into()),
                ..Default::default()
            },
        );
        assert_eq!(s.for_tab(Some(7)).badge_text.as_deref(), Some("9"));
        assert_eq!(s.for_tab(Some(7)).title.as_deref(), Some("Global"));
        assert_eq!(s.for_tab(Some(8)).badge_text.as_deref(), Some("1"));
        assert_eq!(s.for_tab(None).badge_text.as_deref(), Some("1"));
    }
}
