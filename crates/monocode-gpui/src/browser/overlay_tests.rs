//! Overlays GPUI paints on its own (deferred draws, tooltips) cover a page
//! without calling `cover` (K44).
use super::*;
use gpui::{
    Modifiers, TestAppContext, VisualTestContext, anchored, deferred, div, point, px, size,
};

/// A page's place, an overlay above part of it, and a button with a tooltip.
struct Scene {
    overlay: bool,
}

struct Tip;

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(80.)).h(px(24.)).bg(gpui::black()).child("tip")
    }
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(
                div()
                    .id("button")
                    .absolute()
                    .left(px(300.))
                    .top(px(300.))
                    .w(px(40.))
                    .h(px(20.))
                    .tooltip(|_, cx| cx.new(|_| Tip).into()),
            )
            .when(self.overlay, |d| {
                d.child(deferred(
                    anchored()
                        .position(point(px(40.), px(30.)))
                        .child(div().w(px(120.)).h(px(60.)).bg(gpui::white())),
                ))
            })
            // Painting nothing, it covers nothing.
            .child(deferred(div().size_0()))
            .child(sweep())
    }
}

fn close(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    let near = |x: Pixels, y: Pixels| (x - y).abs() < px(1.);
    near(a.origin.x, b.origin.x)
        && near(a.origin.y, b.origin.y)
        && near(a.size.width, b.size.width)
        && near(a.size.height, b.size.height)
}

#[gpui::test]
fn a_deferred_overlay_over_a_page_covers_it(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Scene { overlay: true });
    cx.run_until_parked();
    let swept = take_swept();
    let menu = Bounds::new(point(px(40.), px(30.)), size(px(120.), px(60.)));
    assert_eq!(swept.len(), 1, "{swept:?}");
    assert!(close(swept[0], menu), "{swept:?}");
    let page = Bounds::new(point(px(100.), px(50.)), size(px(400.), px(300.)));
    assert_eq!(shown(true, page, &swept, false), Placement::Covered);
    let beside = Bounds::new(point(px(200.), px(0.)), size(px(80.), px(80.)));
    assert_eq!(shown(true, beside, &swept, false), Placement::Shown);

    view.update(cx, |scene, cx| {
        scene.overlay = false;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(take_swept(), []);
}

#[gpui::test]
fn a_tooltip_over_a_page_covers_it(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Scene { overlay: false });
    cx.run_until_parked();
    let cx: &mut VisualTestContext = cx;
    let mouse = point(px(310.), px(310.));
    cx.simulate_mouse_move(mouse, None, Modifiers::default());
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let swept = take_swept();
    assert_eq!(swept.len(), 1, "{swept:?}");
    let tip = swept[0];
    assert_eq!(tip.size, size(px(80.), px(24.)));
    assert!(tip.origin.x > mouse.x && tip.origin.y > mouse.y, "{tip:?}");
    let page = Bounds::new(point(px(300.), px(320.)), size(px(200.), px(200.)));
    assert_eq!(shown(true, page, &swept, false), Placement::Covered);
}
