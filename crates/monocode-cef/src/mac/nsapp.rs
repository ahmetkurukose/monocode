//! Makes the host's `NSApplication` usable by CEF.
//!
//! There is one `NSApp` per process and the host UI toolkit (GPUI) creates it
//! with its own subclass. CEF requires `NSApp` to conform to `CefAppProtocol`
//! (`isHandlingSendEvent` / `setHandlingSendEvent:` around `sendEvent:`) and
//! dies on a CHECK in `cef_initialize` otherwise. The methods and the protocol
//! conformance are added to whatever class `NSApp` has, through the
//! Objective-C runtime, before CEF is initialized.
//!
//! Approach adapted from bokuweb/gpui-cef (MIT OR Apache-2.0).

use std::{cell::Cell, ffi::c_void};

use objc2::{
    ffi::{
        class_addMethod, class_addProtocol, class_conformsToProtocol, class_getInstanceMethod,
        method_getImplementation, method_setImplementation, objc_allocateProtocol,
        objc_getProtocol, objc_registerProtocol, protocol_addMethodDescription, sel_registerName,
    },
    runtime::{AnyClass, AnyObject, Bool, Sel},
};

thread_local! {
    /// Whether `sendEvent:` is running. `NSApp` lives on the main thread only.
    static HANDLING_SEND_EVENT: Cell<bool> = const { Cell::new(false) };
}

type SendEvent = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

/// The `sendEvent:` in place before ours. Written once, on the main thread,
/// before CEF runs.
static mut ORIGINAL_SEND_EVENT: Option<SendEvent> = None;

extern "C-unwind" fn is_handling_send_event(_this: *mut AnyObject, _sel: Sel) -> Bool {
    Bool::new(HANDLING_SEND_EVENT.with(Cell::get))
}

extern "C-unwind" fn set_handling_send_event(_this: *mut AnyObject, _sel: Sel, handling: Bool) {
    HANDLING_SEND_EVENT.with(|flag| flag.set(handling.as_bool()));
}

extern "C-unwind" fn send_event(this: *mut AnyObject, sel: Sel, event: *mut AnyObject) {
    let previous = HANDLING_SEND_EVENT.with(|flag| flag.replace(true));
    unsafe {
        #[allow(static_mut_refs)]
        if let Some(original) = ORIGINAL_SEND_EVENT {
            original(this, sel, event);
        }
    }
    HANDLING_SEND_EVENT.with(|flag| flag.set(previous));
}

/// Adds `CefAppProtocol` to `NSApp`'s class. Main thread, once, after the host
/// created `NSApp` and before `cef_initialize`.
pub(crate) fn adopt_cef_app_protocol() -> Result<(), &'static str> {
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::NSApplication;

    let mtm = MainThreadMarker::new().ok_or("CEF must start on the main thread")?;
    let app: Retained<NSApplication> = NSApplication::sharedApplication(mtm);
    let class = (&*app as &AnyObject).class() as *const AnyClass as *mut AnyClass;

    unsafe {
        let sel = |name: &std::ffi::CStr| -> Result<Sel, &'static str> {
            sel_registerName(name.as_ptr()).ok_or("failed to register a selector")
        };
        let as_imp = |f: *const c_void| -> unsafe extern "C-unwind" fn() {
            std::mem::transmute::<*const c_void, unsafe extern "C-unwind" fn()>(f)
        };

        let handling = sel(c"isHandlingSendEvent")?;
        if class_getInstanceMethod(class, handling).is_null() {
            class_addMethod(
                class,
                handling,
                as_imp(is_handling_send_event as *const c_void),
                c"c@:".as_ptr(),
            );
            class_addMethod(
                class,
                sel(c"setHandlingSendEvent:")?,
                as_imp(set_handling_send_event as *const c_void),
                c"v@:c".as_ptr(),
            );

            // Wrap sendEvent:. Add an override on the subclass when it has
            // none of its own, so NSApplication itself stays untouched.
            let send_event_sel = sel(c"sendEvent:")?;
            let method = class_getInstanceMethod(class, send_event_sel);
            if method.is_null() {
                return Err("-sendEvent: not found on NSApp's class");
            }
            ORIGINAL_SEND_EVENT = method_getImplementation(method)
                .map(|imp| std::mem::transmute::<unsafe extern "C-unwind" fn(), SendEvent>(imp));
            let ours = as_imp(send_event as *const c_void);
            if !class_addMethod(class, send_event_sel, ours, c"v@:@".as_ptr()).as_bool() {
                method_setImplementation(method, ours);
            }
        }

        // CEF and Chromium check conformsToProtocol:, which compares by name.
        // The protocols exist only in headers, so register them here.
        for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
            let mut protocol = objc_getProtocol(name.as_ptr());
            if protocol.is_null() {
                let allocated = objc_allocateProtocol(name.as_ptr());
                if allocated.is_null() {
                    return Err("failed to allocate a protocol");
                }
                protocol_addMethodDescription(
                    allocated,
                    handling,
                    c"c@:".as_ptr(),
                    Bool::YES,
                    Bool::YES,
                );
                protocol_addMethodDescription(
                    allocated,
                    sel(c"setHandlingSendEvent:")?,
                    c"v@:c".as_ptr(),
                    Bool::YES,
                    Bool::YES,
                );
                objc_registerProtocol(allocated);
                protocol = allocated;
            }
            class_addProtocol(class, protocol);
            if !class_conformsToProtocol(class, protocol).as_bool() {
                return Err("NSApp's class does not conform to the protocol CEF requires");
            }
        }
    }
    Ok(())
}
