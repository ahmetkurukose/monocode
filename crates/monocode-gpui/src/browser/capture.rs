//! Capture (⇧⌘2, the Command Bar's "Capture", the Site Control Center's
//! first tile; docs/native/modes.md §Capture), as SOCode's: the page dims,
//! the element under the pointer is outlined, and a click takes it or a drag
//! takes an area. The page's own picture (CDP `Page.captureScreenshot`, as
//! its snapshots) is cropped to it at the screen's scale, and a menu beside
//! it copies it, saves it into the project's folder, sends it to the chat
//! a grab would go to (K49), or takes another. Esc leaves.
//!
//! The selection draws in Browser mode's child window, over the live page
//! (`shell/overlay_window.rs`); the shell does the saving and the sending
//! ([`PageRequest::SiteControls`]).
use super::*;
use gpui::{Point, point, size};

actions!(
    browser,
    [
        /// ⇧⌘2: captures an area of the page.
        CapturePage
    ]
);

/// A capture under way: the page's frame when it began, and the selection.
pub(crate) struct Capture {
    /// The page's view in the window, as it was drawn when the capture began.
    frame: Bounds<Pixels>,
    url: String,
    pointer: Point<Pixels>,
    /// Where a drag began.
    anchor: Option<Point<Pixels>>,
    /// The element under the pointer.
    hover: Option<Bounds<Pixels>>,
    /// The pointer position last asked about, and whether an answer waits.
    queried: Option<Point<Pixels>>,
    querying: bool,
    selected: Option<Bounds<Pixels>>,
    shot: Option<Captured>,
    copied: bool,
}

/// A captured area: PNG bytes at the screen's scale.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Captured {
    pub png: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// The page it was taken from.
    pub url: String,
    pub title: String,
}

/// Where a capture goes besides the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureTo {
    /// A file in the project's folder.
    Project,
    /// The chat a grab would go to (K49).
    Chat,
}

#[cfg(test)]
thread_local! {
    /// The page's picture the next capture takes, in tests (no Chromium).
    pub(super) static TEST_SHOT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// The rectangle between `a` and `b`, kept inside `frame`.
pub(crate) fn drag_rect(
    a: Point<Pixels>,
    b: Point<Pixels>,
    frame: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let at = |p: Point<Pixels>| {
        point(
            p.x.max(frame.left()).min(frame.right()),
            p.y.max(frame.top()).min(frame.bottom()),
        )
    };
    let (a, b) = (at(a), at(b));
    Bounds::new(
        point(a.x.min(b.x), a.y.min(b.y)),
        size((b.x - a.x).abs(), (b.y - a.y).abs()),
    )
}

/// `rect` (window points) cut out of `png`, a picture of `frame`: at the
/// picture's own scale, so a Retina screen keeps its pixels.
pub(crate) fn crop_png(
    png: &[u8],
    rect: Bounds<Pixels>,
    frame: Bounds<Pixels>,
) -> Option<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let (fw, fh) = (f32::from(frame.size.width), f32::from(frame.size.height));
    if image.width() == 0 || image.height() == 0 || fw <= 0. || fh <= 0. {
        return None;
    }
    let (sx, sy) = (image.width() as f32 / fw, image.height() as f32 / fh);
    let x = ((f32::from(rect.left() - frame.left())) * sx)
        .floor()
        .max(0.) as u32;
    let y = ((f32::from(rect.top() - frame.top())) * sy).floor().max(0.) as u32;
    let right = ((f32::from(rect.right() - frame.left())) * sx)
        .ceil()
        .min(image.width() as f32) as u32;
    let bottom = ((f32::from(rect.bottom() - frame.top())) * sy)
        .ceil()
        .min(image.height() as f32) as u32;
    let (width, height) = (right.checked_sub(x)?, bottom.checked_sub(y)?);
    if width == 0 || height == 0 {
        return None;
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .crop_imm(x, y, width, height)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some((out.into_inner(), width, height))
}

/// The page script that names the element at `(x, y)` (CSS pixels): its
/// box, as `[x, y, width, height]`.
fn element_at(x: f32, y: f32) -> String {
    format!(
        "JSON.stringify((()=>{{const e=document.elementFromPoint({x},{y});\
         if(!e||e===document.documentElement||e===document.body)return null;\
         const r=e.getBoundingClientRect();return[r.x,r.y,r.width,r.height]}})())"
    )
}

impl BrowserPage {
    pub(crate) fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Starts a capture of what the page shows; false when it shows
    /// nothing to capture.
    pub(crate) fn start_capture(&mut self, cx: &mut Context<Self>) -> bool {
        if self.capture.is_some() {
            return true;
        }
        let Some((_, _, frame)) = self.host.painted.get() else {
            return false;
        };
        let shown = cfg!(test) || self.host.view.borrow().is_some();
        if self.url.is_empty() || !shown || frame.size.width <= px(1.) {
            return false;
        }
        self.close_page_popovers(cx);
        self.capture = Some(Capture {
            frame,
            url: self.url.clone(),
            pointer: frame.center(),
            anchor: None,
            hover: None,
            queried: None,
            querying: false,
            selected: None,
            shot: None,
            copied: false,
        });
        cx.notify();
        true
    }

    pub(crate) fn cancel_capture(&mut self, cx: &mut Context<Self>) {
        if self.capture.take().is_some() {
            cx.notify();
        }
    }

    /// The page left the capture's address or moved: it ends.
    fn capture_current(&self) -> bool {
        self.capture.as_ref().is_some_and(|capture| {
            capture.url == self.url
                && (self.host.painted.get()).is_some_and(|(_, _, frame)| frame == capture.frame)
        })
    }

    fn capture_down(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        if capture.selected.is_some() {
            // Beside the menu: the capture ends.
            self.cancel_capture(cx);
            return;
        }
        if !capture.frame.contains(&p) {
            self.cancel_capture(cx);
            return;
        }
        capture.anchor = Some(p);
        capture.pointer = p;
        cx.notify();
    }

    fn capture_move(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        if capture.selected.is_some() {
            return;
        }
        capture.pointer = p;
        cx.notify();
        self.query_capture_hover(cx);
    }

    /// Asks the page which element is under the pointer, one question at
    /// a time.
    fn query_capture_hover(&mut self, cx: &mut Context<Self>) {
        let zoom = f32::from(self.zoom.max(1)) / 100.;
        let Some(capture) = &mut self.capture else {
            return;
        };
        if capture.querying
            || capture.anchor.is_some()
            || capture.queried == Some(capture.pointer)
            || !capture.frame.contains(&capture.pointer)
        {
            return;
        }
        let Some(cdp) = self.cdp() else {
            return;
        };
        let Some(capture) = &mut self.capture else {
            return;
        };
        let at = capture.pointer;
        capture.queried = Some(at);
        capture.querying = true;
        let frame = capture.frame;
        let local = at - frame.origin;
        let expression = element_at(f32::from(local.x) / zoom, f32::from(local.y) / zoom);
        cx.spawn(async move |this, cx| {
            let answer = cdp
                .call(
                    "Runtime.evaluate",
                    serde_json::json!({"expression": expression, "returnByValue": true}),
                    None,
                )
                .await;
            let rect = (answer.ok())
                .and_then(|v| v["result"]["value"].as_str().map(str::to_owned))
                .and_then(|json| serde_json::from_str::<[f32; 4]>(&json).ok())
                .filter(|v| v.iter().all(|n| n.is_finite()) && v[2] > 0. && v[3] > 0.);
            let _ = this.update(cx, |page, cx| {
                let Some(capture) = &mut page.capture else {
                    return;
                };
                capture.querying = false;
                capture.hover = rect.map(|[x, y, w, h]| {
                    let start = frame.origin + point(px(x * zoom), px(y * zoom));
                    let end = start + point(px(w * zoom), px(h * zoom));
                    drag_rect(start, end, frame)
                });
                cx.notify();
                // The pointer moved on meanwhile.
                page.query_capture_hover(cx);
            });
        })
        .detach();
    }

    fn capture_up(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        let Some(anchor) = capture.anchor.take() else {
            return;
        };
        let drag = drag_rect(anchor, p, capture.frame);
        let rect = if drag.size.width > px(3.) && drag.size.height > px(3.) {
            Some(drag)
        } else {
            capture.hover
        };
        let Some(rect) = rect.filter(|r| r.size.width > px(1.) && r.size.height > px(1.)) else {
            cx.notify();
            return;
        };
        capture.selected = Some(rect);
        let frame = capture.frame;
        let (url, title) = (self.url.clone(), self.title.clone());
        let (tx, rx) = async_channel::bounded::<Result<Vec<u8>, String>>(1);
        #[cfg(test)]
        let asked = match TEST_SHOT.with(|shot| shot.borrow_mut().take()) {
            Some(png) => tx.try_send(Ok(png)).is_ok(),
            None => false,
        };
        #[cfg(not(test))]
        let asked = self.host.with_view(|view| {
            view.capture(move |result| {
                let _ = tx.try_send(result);
            })
        });
        if !asked {
            self.cancel_capture(cx);
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let png = rx.recv().await.ok().and_then(Result::ok);
            let cropped = match png {
                Some(png) => {
                    cx.background_spawn(async move { crop_png(&png, rect, frame) })
                        .await
                }
                None => None,
            };
            let _ = this.update(cx, |page, cx| {
                let Some(capture) = &mut page.capture else {
                    return;
                };
                if capture.selected != Some(rect) {
                    return;
                }
                match cropped {
                    Some((png, width, height)) => {
                        capture.shot = Some(Captured {
                            png: Arc::new(png),
                            width,
                            height,
                            url,
                            title,
                        });
                        cx.notify();
                    }
                    None => page.cancel_capture(cx),
                }
            });
        })
        .detach();
    }

    /// The menu's Copy: the picture goes to the clipboard; the menu stays.
    fn copy_capture(&mut self, cx: &mut Context<Self>) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        let Some(shot) = &capture.shot else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            shot.png.as_ref().clone(),
        )));
        capture.copied = true;
        cx.notify();
    }

    /// The menu's Save to Project and Send to Chat: the shell takes it.
    fn send_capture(&mut self, to: CaptureTo, cx: &mut Context<Self>) {
        let Some(shot) = (self.capture.take()).and_then(|capture| capture.shot) else {
            return;
        };
        cx.emit(PageRequest::SiteControls(SiteAsk::Capture(shot, to)));
        cx.notify();
    }

    /// The menu's Retake: back to choosing.
    fn retake_capture(&mut self, cx: &mut Context<Self>) {
        if let Some(capture) = &mut self.capture {
            capture.selected = None;
            capture.shot = None;
            capture.copied = false;
            capture.anchor = None;
            capture.queried = None;
            cx.notify();
        }
    }

    /// The capture's layer: the whole window, the page dimmed but for the
    /// selection, the hint, and the menu once the picture is taken.
    pub(super) fn capture_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let capture = self.capture.as_ref()?;
        if !self.capture_current() {
            // Gone from under it: it ends at the next frame.
            let page = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = page.update(cx, |page, cx| {
                    if !page.capture_current() {
                        page.cancel_capture(cx);
                    }
                });
            });
            return None;
        }
        let frame = capture.frame;
        let selecting = capture.selected.is_none();
        let rect = (capture.selected)
            .or_else(|| (capture.anchor).map(|a| drag_rect(a, capture.pointer, frame)))
            .or(capture.hover);
        let outline = rect.map(|r| {
            Bounds::new(
                r.origin - point(px(3.), px(3.)),
                r.size + size(px(6.), px(6.)),
            )
        });
        let mut layer = div()
            .id("browser-capture")
            .debug_selector(|| "browser-capture".into())
            .size_full()
            .relative()
            .cursor(if selecting {
                CursorStyle::Crosshair
            } else {
                CursorStyle::Arrow
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.capture_down(e.position, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.cancel_capture(cx)
                }),
            )
            .on_mouse_move(
                cx.listener(|this, e: &MouseMoveEvent, _, cx| this.capture_move(e.position, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.capture_up(e.position, cx)),
            )
            .child(mask(frame, outline));
        if let Some(r) = outline {
            layer = layer.child(
                div()
                    .absolute()
                    .left(r.left())
                    .top(r.top())
                    .w(r.size.width)
                    .h(r.size.height)
                    .rounded(px(6.))
                    .border_3()
                    .border_color(crate::theme::accent_color()),
            );
        }
        if selecting {
            layer = layer.child(
                div()
                    .absolute()
                    .left(frame.center().x - px(170.))
                    .top(frame.bottom() - px(84.))
                    .w(px(340.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .bg(crate::theme::pane_fill())
                    .border_1()
                    .border_color(content(0.12))
                    .shadow(crate::popover::shadow())
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(content(0.85))
                    .child("Click or drag to capture a part of this page"),
            );
        } else if let (Some(r), Some(_)) = (rect, &capture.shot) {
            layer = layer.child(self.capture_menu(r, frame, capture.copied, cx));
        }
        // Over everything, from the window's corner: the layer counts in
        // the window's points.
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(point(px(0.), px(0.)))
                    .child(div().w(frame.right()).h(frame.bottom()).child(layer)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn capture_menu(
        &self,
        r: Bounds<Pixels>,
        frame: Bounds<Pixels>,
        copied: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const WIDTH: f32 = 220.;
        const HEIGHT: f32 = 150.;
        let x = if r.right() + px(14. + WIDTH) <= frame.right() {
            r.right() + px(14.)
        } else {
            (r.left() - px(14. + WIDTH)).max(frame.left() + px(8.))
        };
        let y = (r.top() - px(3.))
            .max(frame.top() + px(8.))
            .min((frame.bottom() - px(HEIGHT + 8.)).max(frame.top() + px(8.)));
        let row = |id: &'static str, label: &'static str, path: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Button)
                .aria_label(label)
                .h(px(30.))
                .px(px(8.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(content(0.08)))
                .child(icon(path, 15.).text_color(content(0.6)))
                .child(label)
        };
        div()
            .id("browser-capture-menu")
            .debug_selector(|| "browser-capture-menu".into())
            .absolute()
            .left(x)
            .top(y)
            .w(px(WIDTH))
            .p(px(5.))
            .rounded(px(10.))
            .bg(crate::theme::pane_fill())
            .border_1()
            .border_color(content(0.12))
            .shadow(crate::popover::shadow())
            .text_size(px(13.))
            .text_color(content(0.9))
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                row(
                    "browser-capture-copy",
                    if copied { "Copied" } else { "Copy" },
                    if copied { "check" } else { "copy" },
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy_capture(cx))),
            )
            .child(
                row("browser-capture-save", "Save to Project", "folder").on_click(
                    cx.listener(|this, _, _, cx| this.send_capture(CaptureTo::Project, cx)),
                ),
            )
            .child(
                row("browser-capture-chat", "Send to Chat", "mono-send")
                    .on_click(cx.listener(|this, _, _, cx| this.send_capture(CaptureTo::Chat, cx))),
            )
            .child(div().my(px(4.)).mx(px(6.)).h(px(1.)).bg(content(0.08)))
            .child(
                row("browser-capture-retake", "Retake", "refresh-cw")
                    .on_click(cx.listener(|this, _, _, cx| this.retake_capture(cx))),
            )
            .into_any_element()
    }
}

/// The dim over the page, with a rounded hole where `hole` is.
fn mask(frame: Bounds<Pixels>, hole: Option<Bounds<Pixels>>) -> AnyElement {
    let color = gpui::black().opacity(0.3);
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let mut path = gpui::PathBuilder::fill();
            path.move_to(frame.origin);
            path.line_to(point(frame.right(), frame.top()));
            path.line_to(point(frame.right(), frame.bottom()));
            path.line_to(point(frame.left(), frame.bottom()));
            path.close();
            if let Some(r) = hole {
                let radius = 6_f32
                    .min(f32::from(r.size.width) / 2.)
                    .min(f32::from(r.size.height) / 2.);
                let (l, t, right, bottom) = (
                    f32::from(r.left()),
                    f32::from(r.top()),
                    f32::from(r.right()),
                    f32::from(r.bottom()),
                );
                let mut points = Vec::with_capacity(52);
                for (x, y, from) in [
                    (l + radius, t + radius, std::f32::consts::PI),
                    (right - radius, t + radius, 1.5 * std::f32::consts::PI),
                    (right - radius, bottom - radius, 0.),
                    (l + radius, bottom - radius, std::f32::consts::FRAC_PI_2),
                ] {
                    for i in 0..=12 {
                        let angle = from + std::f32::consts::FRAC_PI_2 * i as f32 / 12.;
                        points.push(point(
                            px(x + radius * angle.cos()),
                            px(y + radius * angle.sin()),
                        ));
                    }
                }
                // The other way round: the hole is taken out of the dim.
                points.reverse();
                if let Some(first) = points.first() {
                    path.move_to(*first);
                    for p in &points[1..] {
                        path.line_to(*p);
                    }
                    path.close();
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 0, 255])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[gpui::test]
    fn a_click_takes_the_element_under_the_pointer_and_send_to_chat_hands_it_on(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(super::super::bind);
        let (page, cx) = cx.add_window_view(|_, cx| {
            let mut page =
                BrowserPage::new("https://a.test/".into(), "A".into(), 100, "/tmp".into(), cx);
            page.nav_in_sidebar = true;
            page
        });
        // The element at (40, 50) in the page is at (10, 20), 30 x 40.
        let (cdp, calls) = super::super::site_controls::tests::fake_cdp(|message| {
            let expression = message["params"]["expression"].as_str().unwrap_or("");
            if expression.contains("elementFromPoint") {
                serde_json::json!({"result": {"type": "string", "value": "[10,20,30,40]"}})
            } else {
                serde_json::json!({})
            }
        });
        let requests = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let requests = requests.clone();
            cx.subscribe(&page, move |_, request: &PageRequest, _| {
                requests.borrow_mut().push(request.clone())
            })
            .detach();
        });
        // The page drawn at (100, 0), 200 x 150, its picture at 2x.
        let frame = bounds(100., 0., 200., 150.);
        page.update_in(cx, |page, window, cx| {
            page.set_cdp_for_test(cdp);
            page.host
                .painted
                .set(Some((window.window_handle().window_id(), 0, frame)));
            window.focus(&page.focus, cx);
        });
        cx.simulate_keystrokes("cmd-shift-2");
        cx.run_until_parked();
        assert!(page.read_with(cx, |page, _| page.capturing()));
        let at = point(px(140.), px(50.));
        cx.simulate_mouse_move(at, None, Default::default());
        cx.run_until_parked();
        assert!(calls.borrow().iter().any(|c| {
            (c["params"]["expression"].as_str())
                .is_some_and(|e| e.contains("elementFromPoint(40,50)"))
        }));
        TEST_SHOT.with(|shot| *shot.borrow_mut() = Some(png(400, 300)));
        cx.simulate_click(at, Default::default());
        cx.run_until_parked();
        let menu = cx.debug_bounds("browser-capture-menu").expect("the menu");
        let send = cx
            .debug_bounds("browser-capture-chat")
            .expect("Send to Chat");
        assert!(menu.contains(&send.center()));
        cx.simulate_click(send.center(), Default::default());
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.capturing()));
        let requests = requests.borrow();
        let [PageRequest::SiteControls(SiteAsk::Capture(shot, CaptureTo::Chat))] =
            requests.as_slice()
        else {
            panic!("one capture: {requests:?}");
        };
        assert_eq!((shot.width, shot.height), (60, 80));
        assert_eq!(shot.title, "A");
    }

    #[gpui::test]
    fn esc_and_a_press_beside_the_page_end_a_capture(cx: &mut gpui::TestAppContext) {
        let (page, cx) = cx.add_window_view(|_, cx| {
            BrowserPage::new(
                "https://a.test/".into(),
                String::new(),
                100,
                "/tmp".into(),
                cx,
            )
        });
        let frame = bounds(100., 0., 200., 150.);
        page.update_in(cx, |page, window, cx| {
            page.host
                .painted
                .set(Some((window.window_handle().window_id(), 0, frame)));
            assert!(page.start_capture(cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("browser-capture").is_some());
        cx.simulate_click(point(px(20.), px(20.)), Default::default());
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.capturing()));
        // A page that moved ends it too.
        page.update_in(cx, |page, window, cx| {
            page.host
                .painted
                .set(Some((window.window_handle().window_id(), 0, frame)));
            assert!(page.start_capture(cx));
            let moved = bounds(90., 0., 200., 150.);
            page.host
                .painted
                .set(Some((window.window_handle().window_id(), 0, moved)));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.capturing()));
    }

    #[test]
    fn a_drag_is_kept_inside_the_page_either_way_round() {
        let frame = bounds(100., 50., 400., 300.);
        let r = drag_rect(point(px(300.), px(200.)), point(px(50.), px(500.)), frame);
        assert_eq!(r, bounds(100., 200., 200., 150.));
    }

    #[test]
    fn the_crop_keeps_the_screens_pixels() {
        // A 2x picture of a 100 x 50 point page drawn at (10, 20).
        let frame = bounds(10., 20., 100., 50.);
        let (bytes, width, height) =
            crop_png(&png(200, 100), bounds(20., 30., 30., 10.), frame).unwrap();
        assert_eq!((width, height), (60, 20));
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        // Its corner is the picture's (20, 20).
        assert_eq!(image.get_pixel(0, 0).0, [20, 20, 0, 255]);
        assert!(crop_png(&png(200, 100), bounds(200., 200., 5., 5.), frame).is_none());
        assert!(crop_png(b"not a png", bounds(20., 30., 30., 10.), frame).is_none());
    }
}
