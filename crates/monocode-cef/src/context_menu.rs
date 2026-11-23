//! The page's context menu (`CefContextMenuHandler`): Chromium's own, with
//! "Send to Agent" added (context.md K49). Choosing it reports where the
//! user right-clicked as [`BrowserEvent::GrabAt`]; the host grabs the
//! element there.

use cef::*;

use crate::view::{BrowserEvent, PageState};

/// Our command: CEF leaves ids from `MENU_ID_USER_FIRST` (26500) to us.
const SEND_TO_AGENT: i32 = 26500;

wrap_context_menu_handler! {
    pub(crate) struct PageContextMenuHandler {
        state: PageState,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            let Some(model) = model else { return };
            if model.count() > 0 {
                model.add_separator();
            }
            model.add_item(SEND_TO_AGENT, Some(&"Send to Agent".into()));
        }

        fn on_context_menu_command(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            command_id: ::std::os::raw::c_int,
            _event_flags: EventFlags,
        ) -> ::std::os::raw::c_int {
            if command_id != SEND_TO_AGENT {
                return 0;
            }
            if let Some(params) = params {
                // In the view's coordinates, relative to the page's origin.
                self.state.send(BrowserEvent::GrabAt {
                    x: params.xcoord(),
                    y: params.ycoord(),
                });
            }
            1
        }
    }
}
