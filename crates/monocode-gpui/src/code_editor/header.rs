//! What outside code adds to the file pane's 30px header: action buttons at
//! its trailing edge, and the crumbs after the file name. The header draws
//! the back/forward arrows and the problem count itself.
use super::{DocumentInfo, symbols::SymbolKind};
use crate::popover::MenuItem;
use gpui::{App, Pixels, Point, SharedString, Window};
use std::rc::Rc;

/// Where and on what a header button was pressed.
#[derive(Clone)]
pub(crate) struct HeaderClick {
    pub doc: Rc<DocumentInfo>,
    /// Window position of the press, to anchor a popover.
    pub position: Point<Pixels>,
    /// The caret, as a byte offset and a zero-based row.
    pub offset: usize,
    pub row: usize,
}

pub(crate) type HeaderHandler = Rc<dyn Fn(&HeaderClick, &mut Window, &mut App)>;
pub(crate) type HeaderMenuItems = Rc<dyn Fn(&App) -> Vec<MenuItem>>;
pub(crate) type HeaderPick = Rc<dyn Fn(&str, &HeaderClick, &mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct HeaderAction {
    /// Stable among one file's actions; it is the button's element id.
    pub id: SharedString,
    /// An icon name from the application's assets, such as "play".
    pub icon: &'static str,
    pub tooltip: SharedString,
    pub enabled: bool,
    /// Draws the button as switched on (a running program, an open panel).
    pub active: bool,
    pub on_click: HeaderHandler,
    /// A dropdown beside the button: the rows to list when it is opened,
    /// and what to do with the id of the row that was picked.
    pub menu: Option<HeaderMenuItems>,
    pub on_pick: Option<HeaderPick>,
}
impl HeaderAction {
    pub(crate) fn new(
        id: impl Into<SharedString>,
        icon: &'static str,
        tooltip: impl Into<SharedString>,
        on_click: impl Fn(&HeaderClick, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            tooltip: tooltip.into(),
            enabled: true,
            active: false,
            on_click: Rc::new(on_click),
            menu: None,
            on_pick: None,
        }
    }
    pub(crate) fn with_menu(
        mut self,
        items: impl Fn(&App) -> Vec<MenuItem> + 'static,
        on_pick: impl Fn(&str, &HeaderClick, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(items));
        self.on_pick = Some(Rc::new(on_pick));
        self
    }
}

/// One segment after the file name. Pressing it moves the caret to `offset`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Crumb {
    pub label: SharedString,
    pub kind: Option<SymbolKind>,
    pub offset: usize,
}

pub(crate) trait HeaderProvider {
    /// Asked on every draw of the header, so it reads state and builds a
    /// few buttons; it does not compute.
    fn actions(&self, doc: &DocumentInfo, cx: &App) -> Vec<HeaderAction>;
    /// Replaces the crumbs the editor derives from the syntax tree (the
    /// enclosing classes and functions of the caret).
    fn crumbs(&self, _doc: &DocumentInfo, _offset: usize, _cx: &App) -> Option<Vec<Crumb>> {
        None
    }
}
