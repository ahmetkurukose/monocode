//! A window of its own for an extension's popup (K69, K44: never GPUI over
//! a page; a native window beside it is fine): borderless, with a shadow
//! and round corners, a child of the app window so it moves with it,
//! anchored under the toolbar button, and able to take the keyboard.

use std::ffi::c_void;

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::AnyObject,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

define_class!(
    /// Borderless windows refuse the keyboard unless they say otherwise.
    #[unsafe(super(NSWindow))]
    #[name = "MonocodeExtensionPopupWindow"]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct PopupWindow;

    unsafe impl NSObjectProtocol for PopupWindow {}

    impl PopupWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }
    }
);

/// A popup's window.
pub struct ExtWindow {
    window: Retained<NSWindow>,
    parent: Option<Retained<NSWindow>>,
    /// The anchor in screen coordinates: the popup's top-right corner.
    top_right: std::cell::Cell<(f64, f64)>,
}

/// Where an anchor is: a point in `view`'s window (top-left based, in
/// points, as GPUI counts), given by any view in that window.
pub struct Anchor {
    pub view: *mut c_void,
    /// The button's right edge and bottom, in the window's content.
    pub right: f64,
    pub bottom: f64,
}

impl ExtWindow {
    /// A popup window, not shown yet, sized `width` × `height`.
    pub fn popup(width: f64, height: f64) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(width, height));
        let this = PopupWindow::alloc(mtm).set_ivars(());
        // SAFETY: main thread; the designated initializer of NSWindow.
        let window: Retained<PopupWindow> = unsafe {
            msg_send![super(this), initWithContentRect: frame, styleMask: NSWindowStyleMask::Borderless, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let window: Retained<NSWindow> = Retained::into_super(window);
        // SAFETY: we keep the window alive ourselves.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setHasShadow(true);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        if let Some(content) = window.contentView() {
            content.setWantsLayer(true);
            // SAFETY: a layer-backed view has a CALayer.
            unsafe {
                let layer: *mut AnyObject = msg_send![&content, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setCornerRadius: 10.0f64];
                    let _: () = msg_send![layer, setMasksToBounds: true];
                }
            }
        }
        Some(ExtWindow {
            window,
            parent: None,
            top_right: std::cell::Cell::new((0., 0.)),
        })
    }

    /// The view pages go in.
    pub fn content_view(&self) -> *mut c_void {
        self.window
            .contentView()
            .map(|v| Retained::as_ptr(&v) as *mut c_void)
            .unwrap_or(std::ptr::null_mut())
    }

    pub fn size(&self) -> (f64, f64) {
        let f = self.window.frame();
        (f.size.width, f.size.height)
    }

    /// Shows the window under `anchor`, its right edge at the anchor's,
    /// kept on the anchor's screen, and gives it the keyboard.
    /// Without a view (no button drawn yet), under the top-right corner of
    /// the app's main window.
    pub fn show(&mut self, anchor: &Anchor) {
        // SAFETY: the app passes a live NSView of its window, main thread.
        let parent = match unsafe { Retained::retain(anchor.view.cast::<NSView>()) } {
            Some(view) => view.window(),
            None => MainThreadMarker::new().and_then(|mtm| {
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                app.mainWindow().or_else(|| app.windows().firstObject())
            }),
        };
        let Some(parent) = parent else { return };
        let Some(content) = parent.contentView() else {
            return;
        };
        let bounds = content.bounds();
        let (right, bottom) = if anchor.view.is_null() {
            (bounds.size.width - 12., 80.)
        } else {
            (anchor.right, anchor.bottom)
        };
        let y = if content.isFlipped() {
            bottom
        } else {
            bounds.size.height - bottom
        };
        let in_window = content.convertPoint_toView(NSPoint::new(right, y), None);
        let on_screen = parent.convertPointToScreen(in_window);
        self.top_right.set((on_screen.x, on_screen.y));
        self.place();
        // SAFETY: main thread; both windows live.
        unsafe { parent.addChildWindow_ordered(&self.window, NSWindowOrderingMode::Above) };
        self.window.makeKeyAndOrderFront(None);
        self.parent = Some(parent);
    }

    fn place(&self) {
        let (w, h) = self.size();
        let (right, top) = self.top_right.get();
        let mut origin = NSPoint::new(right - w, top - h);
        if let Some(screen) = self
            .parent
            .as_ref()
            .and_then(|p| p.screen())
            .or_else(|| self.window.screen())
        {
            let visible = screen.visibleFrame();
            origin.x = origin.x.max(visible.origin.x + 4.);
            origin.y = origin.y.max(visible.origin.y + 4.);
        }
        self.window
            .setFrame_display(NSRect::new(origin, NSSize::new(w, h)), true);
    }

    /// Resizes the window, its top-right corner staying where it is.
    pub fn set_size(&self, width: f64, height: f64) {
        let f = self.window.frame();
        if (f.size.width - width).abs() < 0.5 && (f.size.height - height).abs() < 0.5 {
            return;
        }
        self.window
            .setFrame_display(NSRect::new(f.origin, NSSize::new(width, height)), false);
        if self.parent.is_some() {
            self.place();
        }
    }

    pub fn is_key(&self) -> bool {
        self.window.isKeyWindow()
    }

    pub fn close(&mut self) {
        if let Some(parent) = self.parent.take() {
            parent.removeChildWindow(&self.window);
            // The app window takes the keyboard back.
            parent.makeKeyWindow();
        }
        self.window.orderOut(None);
        self.window.close();
    }
}

impl Drop for ExtWindow {
    fn drop(&mut self) {
        if self.window.isVisible() {
            self.close();
        }
    }
}
