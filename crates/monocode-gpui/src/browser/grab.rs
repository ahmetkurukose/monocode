//! Element grab in a browser page (docs/native/browser/context.md K49): the
//! toolbar's button or ⌥⌘G shows a picker over the page; a click picks an
//! element, Esc or the button again leaves. The context menu's "Send to
//! Agent" grabs the element under the pointer. Either way the page asks
//! its pane's shell to deliver the result ([`PageRequest::Grab`]), which
//! puts it in an agent's composer (`shell/browser_grab.rs`).
//!
//! The picking and the description are `monocode_core::browser::grab`'s.

use super::*;
use monocode_core::browser::grab::{Grab, Picker, grab_at, grab_inspected};

actions!(
    browser,
    [
        /// ⌥⌘G: picks an element of the page for an agent, or stops picking.
        ToggleGrab
    ]
);

/// A page's grab mode: on from the button or shortcut until a pick, Esc,
/// the button again, or a navigation.
#[derive(Default)]
pub(crate) struct GrabMode {
    on: bool,
    /// Set once the picker shows over the page.
    picker: Option<Rc<Picker>>,
    task: Option<gpui::Task<()>>,
}

/// What a grab gives the shell: the element, or why there is none.
pub(crate) type GrabResult = Result<Rc<Grab>, String>;

impl BrowserPage {
    #[cfg(test)]
    pub(crate) fn grabbing(&self) -> bool {
        self.grab.on
    }

    /// Starts picking, or stops.
    pub(crate) fn toggle_grab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.grab.on {
            self.cancel_grab(cx);
            return;
        }
        let Some(cdp) = self.cdp() else {
            return;
        };
        self.grab.on = true;
        self.grab.task = Some(cx.spawn(async move |this, cx| {
            let picker = match Picker::arm(cdp).await {
                Ok(picker) => Rc::new(picker),
                Err(error) => {
                    let _ = this.update(cx, |page, cx| page.end_grab(Some(Err(error)), cx));
                    return;
                }
            };
            let armed = this.update(cx, |page, _| {
                let on = page.grab.on;
                if on {
                    page.grab.picker = Some(picker.clone());
                }
                on
            });
            if !armed.unwrap_or(false) {
                // Left while the picker was coming up.
                picker.cancel();
                return;
            }
            let picked = picker.wait().await;
            let _ = this.update(cx, |page, cx| {
                page.end_grab(picked.map(|g| g.map(Rc::new)).transpose(), cx)
            });
        }));
        // Esc reaches the picker in the page.
        window.focus(&self.focus, cx);
        self.host.with_view(|view| view.set_focus(true));
        cx.notify();
    }

    /// Leaves grab mode without a pick.
    pub(crate) fn cancel_grab(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.grab.picker.take() {
            picker.cancel();
        }
        self.end_grab(None, cx);
    }

    fn end_grab(&mut self, result: Option<GrabResult>, cx: &mut Context<Self>) {
        let was_on = std::mem::take(&mut self.grab.on);
        self.grab.picker = None;
        // Dropping the task from inside itself is fine: it is finishing.
        self.grab.task = None;
        if was_on && let Some(result) = result {
            self.emit_grab(result, cx);
        }
        cx.notify();
    }

    /// Hands a grab to the shell, the site's saved passwords masked (K57).
    fn emit_grab(&mut self, result: GrabResult, cx: &mut Context<Self>) {
        let secrets = self.agent_secrets(cx);
        let result = result.map(|grab| {
            if secrets.is_empty() {
                grab
            } else {
                Rc::new((*grab).clone().masked(&secrets))
            }
        });
        cx.emit(PageRequest::Grab(result));
    }

    /// The context menu's "Send to Agent", at `(x, y)` in the view.
    pub(crate) fn grab_at(&mut self, x: i32, y: i32, cx: &mut Context<Self>) {
        let Some(cdp) = self.cdp() else {
            return;
        };
        // The view counts in points; the page in CSS pixels, scaled by zoom.
        let zoom = f64::from(self.zoom.max(1)) / 100.;
        let (x, y) = (f64::from(x) / zoom, f64::from(y) / zoom);
        cx.spawn(async move |this, cx| {
            let result = grab_at(cdp, x, y).await.map(Rc::new);
            let _ = this.update(cx, |page, cx| page.emit_grab(result, cx));
        })
        .detach();
    }

    /// The docked DevTools' "Send Selected Element to Agent": the element
    /// selected in their Elements panel, which is `$0` in their own session
    /// on the page.
    pub(crate) fn grab_selected(&mut self, cx: &mut Context<Self>) {
        let Some(cdp) = self.cdp() else {
            return;
        };
        let session = monocode_core::browser::proxy::devtools_session(&cdp);
        cx.spawn(async move |this, cx| {
            let result = match session {
                Some(session) => grab_inspected(cdp, &session).await,
                None => Err("DevTools are not connected to the page".to_owned()),
            };
            let _ = this.update(cx, |page, cx| page.emit_grab(result.map(Rc::new), cx));
        })
        .detach();
    }

    /// The toolbar's button: on while picking.
    pub(super) fn grab_button(&self, ready: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.grab.on;
        icon_button_enabled(
            "browser-grab",
            if on {
                "Stop Picking"
            } else {
                "Send an Element to Agent"
            },
            "grab",
            ready,
        )
        .debug_selector(|| "browser-grab".into())
        .when(on, |b| b.bg(content(0.1)).text_color(content(1.)))
        .on_click(cx.listener(|this, _, window, cx| this.toggle_grab(window, cx)))
    }

    /// Esc while GPUI has the keyboard; in the page the picker hears it.
    pub(super) fn grab_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.grab.on && event.keystroke.key == "escape" {
            self.cancel_grab(cx);
            cx.stop_propagation();
        }
    }
}

#[cfg(test)]
impl BrowserPage {
    /// Gives the page a CDP client, as its view would.
    pub(crate) fn set_cdp_for_test(&self, cdp: Rc<CdpClient>) {
        let log = PageLog::start(&cdp);
        *self.host.agent.borrow_mut() = Some(AgentLink {
            cdp,
            log,
            _attachment: Box::new(()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A page's DevTools agent that answers at once, except the picker's
    /// `wait()`, which waits for [`Peer::answer_wait`] or a cancel.
    #[derive(Default)]
    struct Peer {
        client: RefCell<Weak<CdpClient>>,
        waiting: RefCell<Vec<Value>>,
        expressions: RefCell<Vec<String>>,
    }

    impl Peer {
        fn client() -> (Rc<CdpClient>, Rc<Peer>) {
            let peer = Rc::new(Peer::default());
            let answers = peer.clone();
            let client = CdpClient::new(Box::new(move |bytes| {
                answers.take(serde_json::from_slice(bytes).unwrap());
                true
            }));
            *peer.client.borrow_mut() = Rc::downgrade(&client);
            (client, peer)
        }

        fn reply(&self, id: &Value, result: Value) {
            let client = self.client.borrow().upgrade().unwrap();
            let answer = json!({"id": id, "result": result});
            client.on_message(Some(answer.to_string().as_bytes()));
        }

        fn take(&self, message: Value) {
            let id = &message["id"];
            let expression = message["params"]["expression"].as_str().unwrap_or("");
            self.expressions.borrow_mut().push(expression.to_owned());
            if expression.ends_with(".wait()") {
                self.waiting.borrow_mut().push(id.clone());
                return;
            }
            let result = match message["method"].as_str().unwrap_or("") {
                "Page.getFrameTree" => json!({"frameTree": {"frame": {"id": "F"}}}),
                "Page.createIsolatedWorld" => json!({"executionContextId": 3}),
                "Runtime.evaluate" if expression.ends_with(".picked") => {
                    json!({"result": {"type": "object", "objectId": "el"}})
                }
                "Runtime.callFunctionOn" => json!({"result": {"value": {
                    "tag": "button",
                    "selector": "button.go",
                    "rect": {"x": 0, "y": 0, "width": 10, "height": 10},
                }}}),
                _ => json!({"result": {"type": "boolean", "value": true}}),
            };
            self.reply(id, result);
            if expression.contains(".cancel()") {
                self.answer_wait("cancelled");
            }
        }

        fn answer_wait(&self, how: &str) {
            let waiting: Vec<Value> = self.waiting.borrow_mut().drain(..).collect();
            for id in waiting {
                self.reply(&id, json!({"result": {"type": "string", "value": how}}));
            }
        }

        fn asked(&self, expression: &str) -> bool {
            self.expressions.borrow().iter().any(|e| e == expression)
        }
    }

    #[gpui::test]
    fn the_shortcut_and_the_button_start_and_stop_picking(cx: &mut gpui::TestAppContext) {
        cx.update(super::super::bind);
        let (page, cx) = cx.add_window_view(|_, cx| {
            BrowserPage::new(
                "https://a.test/".into(),
                String::new(),
                100,
                "/tmp".into(),
                cx,
            )
        });
        let requests = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let requests = requests.clone();
            cx.subscribe(&page, move |_, request: &PageRequest, _| {
                requests.borrow_mut().push(request.clone())
            })
            .detach();
        });
        let (cdp, peer) = Peer::client();
        page.update_in(cx, |page, window, cx| {
            page.set_cdp_for_test(cdp);
            window.focus(&page.focus, cx);
        });

        // ⌥⌘G shows the picker; Esc takes it down.
        cx.simulate_keystrokes("cmd-alt-g");
        cx.run_until_parked();
        assert!(page.read_with(cx, |page, _| page.grabbing()));
        assert!(peer.asked("window.__monocodeGrab.arm()"));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.grabbing()));
        assert!(peer.asked("window.__monocodeGrab && window.__monocodeGrab.cancel()"));

        // The toolbar's button: on, off, on again; a pick ends it.
        let button = cx.debug_bounds("browser-grab").expect("the grab button");
        cx.simulate_click(button.center(), Default::default());
        cx.run_until_parked();
        assert!(page.read_with(cx, |page, _| page.grabbing()));
        cx.simulate_click(button.center(), Default::default());
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.grabbing()));
        assert!(requests.borrow().is_empty(), "leaving sends nothing");

        cx.simulate_click(button.center(), Default::default());
        cx.run_until_parked();
        peer.answer_wait("picked");
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.grabbing()));
        let requests = requests.borrow();
        let [PageRequest::Grab(Ok(grab))] = requests.as_slice() else {
            panic!("one grab: {requests:?}");
        };
        assert_eq!(grab.element.selector, "button.go");
    }
}
