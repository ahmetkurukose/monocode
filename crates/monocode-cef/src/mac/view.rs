//! Placing CEF's `NSView` inside its parent.

use std::ffi::c_void;

use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::view::Frame;

fn view(native: *mut c_void) -> Option<Retained<NSView>> {
    MainThreadMarker::new()?;
    // SAFETY: `native` is the NSView CEF created for the browser; it lives as
    // long as the browser and is only touched on the main thread.
    unsafe { Retained::retain(native.cast::<NSView>()) }
}

/// `frame` is top-left based; AppKit views are bottom-left based unless the
/// parent is flipped.
pub(crate) fn set_frame(native: *mut c_void, frame: Frame) {
    let Some(view) = view(native) else { return };
    let y = match unsafe { view.superview() } {
        Some(parent) if !parent.isFlipped() => parent.bounds().size.height - frame.y - frame.height,
        _ => frame.y,
    };
    view.setFrame(NSRect::new(
        NSPoint::new(frame.x, y),
        NSSize::new(frame.width.max(1.0), frame.height.max(1.0)),
    ));
}

/// A hidden parent for a popup CEF creates before the host has a place for
/// it (crate::popup).
pub(crate) fn hidden_parent(width: f64, height: f64) -> Option<Retained<NSView>> {
    let mtm = MainThreadMarker::new()?;
    let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(width, height));
    let parent = NSView::initWithFrame(NSView::alloc(mtm), frame);
    parent.setHidden(true);
    Some(parent)
}

/// Moves CEF's view into `parent`, an `NSView*` of the host.
pub(crate) fn reparent(native: *mut c_void, parent: *mut c_void) {
    let Some(view) = view(native) else { return };
    // SAFETY: the host passes a live NSView, on the main thread.
    let Some(parent) = (unsafe { Retained::retain(parent.cast::<NSView>()) }) else {
        return;
    };
    // Moving keeps the view alive: it is retained here while it changes
    // superviews.
    parent.addSubview(&view);
}

/// Takes CEF's view out of its parent, which lets it go: a closing browser
/// is destroyed with its view.
pub(crate) fn detach(native: *mut c_void) {
    if let Some(view) = view(native) {
        view.removeFromSuperview();
    }
}

pub(crate) fn set_hidden(native: *mut c_void, hidden: bool) {
    if let Some(view) = view(native) {
        view.setHidden(hidden);
    }
}
