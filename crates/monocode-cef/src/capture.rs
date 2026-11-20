//! The last frame of a browser, as a PNG (context.md K16): CDP
//! `Page.captureScreenshot` sent through `ExecuteDevToolsMethod`, its result
//! received by a DevTools message observer.
//!
//! CEF calls the observer on its UI thread, which is the main thread here, so
//! the waiting callbacks live in a thread-local and need not be `Send`.

use std::{cell::RefCell, collections::HashMap};

use base64::Engine as _;
use cef::*;

/// Receives a capture: PNG bytes, or why there are none.
pub type CaptureDone = Box<dyn FnOnce(Result<Vec<u8>, String>)>;

/// Our message ids start high, so they never meet the small ids another
/// DevTools client of the same browser counts from.
const FIRST_ID: i32 = 0x4000_0000;

#[derive(Default)]
struct Captures {
    /// One observer per browser, by browser id; dropping it unregisters.
    observers: HashMap<i32, Registration>,
    /// Waiting captures, by browser id and message id.
    waiting: HashMap<(i32, i32), CaptureDone>,
    next_id: i32,
}

thread_local! {
    static CAPTURES: RefCell<Captures> = RefCell::new(Captures::default());
}

wrap_dev_tools_message_observer! {
    struct CaptureObserver {
        browser: i32,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            let done = CAPTURES.with(|c| c.borrow_mut().waiting.remove(&(self.browser, message_id)));
            // Not ours: another DevTools client of this browser.
            let Some(done) = done else { return };
            let result = result.unwrap_or_default();
            done(if success != 0 {
                screenshot_png(result)
            } else {
                Err(String::from_utf8_lossy(result).into_owned())
            });
        }
    }
}

/// Asks `browser` for a PNG of what it shows; `done` runs on the main thread.
/// `done` is dropped without running if the browser goes first.
pub(crate) fn capture(browser: &Browser, done: CaptureDone) {
    let Some(host) = browser.host() else {
        return done(Err("the browser has no host".into()));
    };
    let browser_id = browser.identifier();
    let message_id = CAPTURES.with(|c| {
        let mut c = c.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(entry) = c.observers.entry(browser_id)
            && let Some(registration) =
                host.add_dev_tools_message_observer(Some(&mut CaptureObserver::new(browser_id)))
        {
            entry.insert(registration);
        }
        c.next_id = c.next_id.max(FIRST_ID).wrapping_add(1).max(FIRST_ID);
        let id = c.next_id;
        // Waiting before the call: a result may come back inside it.
        c.waiting.insert((browser_id, id), done);
        id
    });
    let params = dictionary_value_create();
    if let Some(params) = &params {
        params.set_string(Some(&"format".into()), Some(&"png".into()));
        // Faster encoding, bigger files: the bytes never leave the app.
        params.set_bool(Some(&"optimizeForSpeed".into()), 1);
    }
    let mut params = params;
    let sent = host.execute_dev_tools_method(
        message_id,
        Some(&"Page.captureScreenshot".into()),
        params.as_mut(),
    );
    if sent == 0 {
        let done = CAPTURES.with(|c| c.borrow_mut().waiting.remove(&(browser_id, message_id)));
        if let Some(done) = done {
            done(Err("Chromium did not take the capture request".into()));
        }
    }
}

/// Forgets a closing browser's observer and its waiting captures.
pub(crate) fn forget(browser_id: i32) {
    let (observer, waiting) = CAPTURES.with(|c| {
        let mut c = c.borrow_mut();
        let observer = c.observers.remove(&browser_id);
        let keys: Vec<_> = c
            .waiting
            .keys()
            .filter(|(browser, _)| *browser == browser_id)
            .copied()
            .collect();
        let waiting: Vec<_> = keys.iter().filter_map(|k| c.waiting.remove(k)).collect();
        (observer, waiting)
    });
    // Dropped outside the borrow: either may call back into CEF.
    drop(observer);
    drop(waiting);
}

/// The PNG in a `Page.captureScreenshot` result (`{"data": "<base64>"}`).
pub(crate) fn screenshot_png(result: &[u8]) -> Result<Vec<u8>, String> {
    #[derive(serde::Deserialize)]
    struct Screenshot {
        data: String,
    }
    let screenshot: Screenshot =
        serde_json::from_slice(result).map_err(|e| format!("unexpected capture result: {e}"))?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(screenshot.data.as_bytes())
        .map_err(|e| format!("capture is not base64: {e}"))?;
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !png.starts_with(SIGNATURE) {
        return Err("capture is not a PNG".into());
    }
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screenshot_result_gives_its_png() {
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let result = format!(r#"{{"data":"{data}"}}"#);
        assert_eq!(screenshot_png(result.as_bytes()), Ok(png));
    }

    #[test]
    fn a_result_without_a_png_is_an_error() {
        assert!(screenshot_png(b"{}").is_err());
        assert!(screenshot_png(b"not json").is_err());
        assert!(screenshot_png(br#"{"data":"%%%"}"#).is_err());
        let gif = base64::engine::general_purpose::STANDARD.encode(b"GIF89a");
        assert!(screenshot_png(format!(r#"{{"data":"{gif}"}}"#).as_bytes()).is_err());
    }
}
