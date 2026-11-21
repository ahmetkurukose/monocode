//! A page's raw DevTools channel (context.md §6.1, K38): CDP messages in and
//! out as JSON bytes, for the agent tools and the external CDP endpoint.
//!
//! The channel knows nothing of the protocol. Ids, sessions and who asked
//! what are the business of the multiplexer in `monocode_core::browser::cdp`,
//! which is the page's only user of it. Our message ids stay below
//! `0x4000_0000`; page captures (`crate::capture`) count from there.
//!
//! CEF calls the observer on its UI thread, the main thread here, so the
//! sink need not be `Send`.

use cef::*;

/// Receives every DevTools message of a page: results (`{"id":…}`) and
/// events (`{"method":…}`), in order, on the main thread. `None` means the
/// page's DevTools agent went away (the renderer crashed or the page
/// closed); messages sent before it get no answer.
pub type DevToolsSink = Box<dyn Fn(Option<&[u8]>)>;

wrap_dev_tools_message_observer! {
    struct RawObserver {
        sink: std::rc::Rc<DevToolsSink>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&[u8]>,
        ) -> ::std::os::raw::c_int {
            (self.sink)(Some(message.unwrap_or_default()));
            // Handled: CEF need not parse it again for this observer.
            1
        }

        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            (self.sink)(None);
        }
    }
}

/// Keeps a sink attached; dropping it detaches.
pub struct DevToolsAttachment {
    _registration: Registration,
}

/// Attaches `sink` to `browser`'s DevTools messages.
pub(crate) fn attach(browser: &Browser, sink: DevToolsSink) -> Option<DevToolsAttachment> {
    let host = browser.host()?;
    let mut observer = RawObserver::new(std::rc::Rc::new(sink));
    let registration = host.add_dev_tools_message_observer(Some(&mut observer))?;
    Some(DevToolsAttachment {
        _registration: registration,
    })
}

/// Sends one CDP message (`{"id":…,"method":…,"params":…,"sessionId"?:…}`).
/// False when Chromium did not take it.
pub(crate) fn send(browser: &Browser, message: &[u8]) -> bool {
    browser
        .host()
        .is_some_and(|host| host.send_dev_tools_message(Some(message)) != 0)
}
