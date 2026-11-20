//! Placing CEF's `NSView` inside its parent.

use std::ffi::c_void;

use objc2::{MainThreadMarker, rc::Retained};
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

pub(crate) fn set_hidden(native: *mut c_void, hidden: bool) {
    if let Some(view) = view(native) {
        view.setHidden(hidden);
    }
}
