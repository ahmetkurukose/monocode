//! The page's context menu (`CefContextMenuHandler`): Chromium's own, with
//! "Send to Agent" added (context.md K49). Choosing it reports where the
//! user right-clicked as [`BrowserEvent::GrabAt`]; the host grabs the
//! element there. A page's own menu (the one with Back) always has
//! "Print…", which prints as ⌘P does.

use cef::*;

use crate::view::{BrowserEvent, PageState};

/// Our command: CEF leaves ids from `MENU_ID_USER_FIRST` (26500) to us.
const SEND_TO_AGENT: i32 = 26500;

/// The item's label in a frame at `url`. In the docked DevTools (a page of
/// their own) it sends the element selected in their Elements panel, not
/// what is under the pointer; the host tells the two apart by the view.
const BACK: i32 = sys::cef_menu_id_t::MENU_ID_BACK as i32;
const PRINT: i32 = sys::cef_menu_id_t::MENU_ID_PRINT as i32;

/// Whether "Print…" joins a menu: the page's own (it has Back), unless
/// Chromium put it there, and not the docked DevTools'.
fn adds_print(has_back: bool, has_print: bool, devtools: bool) -> bool {
    has_back && !has_print && !devtools
}

fn devtools(url: Option<&str>) -> bool {
    url.is_some_and(|u| u.starts_with("devtools://"))
}

fn label(url: Option<&str>) -> &'static str {
    if devtools(url) {
        "Send Selected Element to Agent"
    } else {
        "Send to Agent"
    }
}

wrap_context_menu_handler! {
    pub(crate) struct PageContextMenuHandler {
        state: PageState,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            let Some(model) = model else { return };
            let url = frame.map(|f| CefString::from(&f.url()).to_string());
            let url = url.as_deref();
            let (back, print) = (model.index_of(BACK) >= 0, model.index_of(PRINT) >= 0);
            if adds_print(back, print, devtools(url)) {
                model.add_separator();
                model.add_item(PRINT, Some(&"Print…".into()));
            }
            if model.count() > 0 {
                model.add_separator();
            }
            model.add_item(SEND_TO_AGENT, Some(&label(url).into()));
        }

        fn on_context_menu_command(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            command_id: ::std::os::raw::c_int,
            _event_flags: EventFlags,
        ) -> ::std::os::raw::c_int {
            if command_id == PRINT {
                if let Some(host) = browser.and_then(|b| b.host()) {
                    host.print();
                }
                return 1;
            }
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_pages_own_menu_always_prints() {
        assert!(super::adds_print(true, false, false));
        // Chromium's own item stays; a field's or a selection's menu (no
        // Back) and the DevTools' get none.
        assert!(!super::adds_print(true, true, false));
        assert!(!super::adds_print(false, false, false));
        assert!(!super::adds_print(true, false, true));
    }

    #[test]
    fn devtools_send_their_selected_element() {
        assert_eq!(super::label(Some("https://a.test/")), "Send to Agent");
        assert_eq!(super::label(None), "Send to Agent");
        assert_eq!(
            super::label(Some("devtools://devtools/bundled/devtools_app.html?ws=x")),
            "Send Selected Element to Agent"
        );
    }
}
