//! Drives CEF's message loop from the main run loop (`external_message_pump`).
//!
//! The host toolkit owns `[NSApp run]`, so CEF cannot run its own loop. CEF
//! says when it wants `cef_do_message_loop_work()` through
//! `OnScheduleMessagePumpWork(delay)`, from any thread. That call only records
//! a deadline and signals a run-loop source; the source, on the main thread,
//! moves a `CFRunLoopTimer` forward; the timer does the work. Work therefore
//! never runs inside the host's own callbacks (no borrow of its state is live)
//! and never re-enters itself: a nested run loop (a context menu, a modal
//! panel) that fires the timer while work is running only reschedules it.
//!
//! The timer is installed in `kCFRunLoopCommonModes`, so it keeps firing while
//! a window is being resized or a menu is tracking. A fallback fire every
//! [`MAX_DELAY`] covers missed requests, as CEF's own external pump does.

use std::{
    cell::Cell,
    ffi::c_void,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
    time::{Duration, Instant},
};

/// Longest gap between two work calls (CEF's cefclient uses 1/30 s).
const MAX_DELAY: Duration = Duration::from_millis(33);

type CFTypeRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopTimerRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type CFAbsoluteTime = f64;
type CFTimeInterval = f64;

#[repr(C)]
struct CFRunLoopTimerContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

#[repr(C)]
struct CFRunLoopSourceContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<extern "C" fn(*const c_void) -> usize>,
    schedule: Option<extern "C" fn(*const c_void, CFRunLoopRef, CFStringRef)>,
    cancel: Option<extern "C" fn(*const c_void, CFRunLoopRef, CFStringRef)>,
    perform: Option<extern "C" fn(*const c_void)>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;
    fn CFRunLoopTimerCreate(
        allocator: CFTypeRef,
        fire_date: CFAbsoluteTime,
        interval: CFTimeInterval,
        flags: usize,
        order: isize,
        callout: extern "C" fn(CFRunLoopTimerRef, *mut c_void),
        context: *mut CFRunLoopTimerContext,
    ) -> CFRunLoopTimerRef;
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, fire_date: CFAbsoluteTime);
    fn CFRunLoopSourceCreate(
        allocator: CFTypeRef,
        order: isize,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
}

/// The latest deadline CEF asked for. Written from any thread.
static REQUESTED: Mutex<Option<Instant>> = Mutex::new(None);
/// The process-lifetime wake source (never released).
static SOURCE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// The process-lifetime timer (never released). Main thread only.
static TIMER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

thread_local! {
    static PUMPING: Cell<bool> = const { Cell::new(false) };
    static AGAIN: Cell<bool> = const { Cell::new(false) };
}

/// From `OnScheduleMessagePumpWork`; any thread.
pub(crate) fn schedule_work(delay_ms: i64) {
    let delay = Duration::from_millis(delay_ms.clamp(0, MAX_DELAY.as_millis() as i64) as u64);
    let deadline = Instant::now() + delay;
    {
        let mut requested = REQUESTED.lock().unwrap_or_else(|e| e.into_inner());
        *requested = Some(requested.map_or(deadline, |d| d.min(deadline)));
    }
    let source = SOURCE.load(Ordering::Acquire);
    if !source.is_null() {
        unsafe {
            CFRunLoopSourceSignal(source);
            CFRunLoopWakeUp(CFRunLoopGetMain());
        }
    }
}

fn fire_in(delay: Duration) {
    let timer = TIMER.load(Ordering::Acquire);
    if !timer.is_null() {
        unsafe {
            CFRunLoopTimerSetNextFireDate(timer, CFAbsoluteTimeGetCurrent() + delay.as_secs_f64())
        };
    }
}

/// Main thread: bring the timer forward to the requested deadline.
extern "C" fn on_signal(_info: *const c_void) {
    let deadline = REQUESTED.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(deadline) = deadline {
        fire_in(deadline.saturating_duration_since(Instant::now()));
    }
}

extern "C" fn on_timer(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    if PUMPING.with(|p| p.replace(true)) {
        // Re-entered from a nested run loop inside the work below.
        AGAIN.with(|a| a.set(true));
        return;
    }
    // The fallback; a request made during the work moves it earlier.
    fire_in(MAX_DELAY);
    cef::do_message_loop_work();
    PUMPING.with(|p| p.set(false));
    if AGAIN.with(|a| a.replace(false)) {
        fire_in(Duration::ZERO);
    }
}

/// Installs the source and the timer on the main run loop. Main thread, once.
pub(crate) fn install() {
    if !TIMER.load(Ordering::Acquire).is_null() {
        return;
    }
    unsafe {
        let main = CFRunLoopGetMain();
        let mut source_context = CFRunLoopSourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(on_signal),
        };
        let source = CFRunLoopSourceCreate(ptr::null(), 0, &mut source_context);
        CFRunLoopAddSource(main, source, kCFRunLoopCommonModes);
        SOURCE.store(source, Ordering::Release);

        let mut timer_context = CFRunLoopTimerContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
        };
        // A repeating interval far in the future: the fire date is always set
        // explicitly; the interval only keeps the timer valid after it fires.
        let timer = CFRunLoopTimerCreate(
            ptr::null(),
            CFAbsoluteTimeGetCurrent() + MAX_DELAY.as_secs_f64(),
            1.0e9,
            0,
            0,
            on_timer,
            &mut timer_context,
        );
        CFRunLoopAddTimer(main, timer, kCFRunLoopCommonModes);
        TIMER.store(timer, Ordering::Release);
    }
}
