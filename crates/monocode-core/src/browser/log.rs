//! A page's console and network log (context.md K36), kept from the page's
//! first load: ring buffers fed by CDP events.
//!
//! STUB: the agent-tools work fills this in.

use std::rc::Rc;

use super::cdp::CdpClient;

pub struct PageLog {
    _cdp: std::rc::Weak<CdpClient>,
}

impl PageLog {
    /// Enables the domains the log needs on `cdp` and starts recording.
    pub fn start(cdp: &Rc<CdpClient>) -> Rc<PageLog> {
        Rc::new(PageLog {
            _cdp: Rc::downgrade(cdp),
        })
    }
}
