//! Footer chips a feature contributes, beside the resource and running
//! terminal chips: an icon, a few words and a state colour, opening a list on
//! click. A chip is drawn while its `state` returns something and is absent
//! otherwise, so a feature with nothing to report takes no room.
use gpui::{App, Global, SharedString, Window};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Nothing to remark on.
    #[default]
    Quiet,
    /// Work in progress.
    Busy,
    Good,
    Warning,
    Error,
}

/// What a chip shows right now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChipState {
    pub icon: &'static str,
    pub text: SharedString,
    pub tone: Tone,
    /// Read out and shown on hover.
    pub tooltip: SharedString,
}

/// A row of a chip's list.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChipItem {
    /// Passed to `FooterChip::pick` when the row is chosen.
    pub id: SharedString,
    pub title: SharedString,
    /// Shown dimmed at the row's end: a state, memory, a port.
    pub detail: Option<SharedString>,
    pub icon: Option<&'static str>,
    pub tone: Tone,
    /// What can be done to the row, as `(id, label)`: "Stop", "Restart". With
    /// any, the row opens them as a submenu and `pick` gets the chosen id.
    pub actions: Vec<(SharedString, SharedString)>,
}

/// What a chip shows now; `None` hides it.
pub(crate) type ChipSource = Rc<dyn Fn(&App) -> Option<ChipState>>;
pub(crate) type ChipItems = Rc<dyn Fn(&App) -> Vec<ChipItem>>;
pub(crate) type ChipPick = Rc<dyn Fn(&str, &mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct FooterChip {
    /// Fixed and unique, such as `"code-running"`.
    pub id: &'static str,
    /// Names the list for assistive technology.
    pub label: &'static str,
    /// Called while the footer draws: cheap, and without side effects.
    pub state: ChipSource,
    /// The list's rows, read when the chip is clicked.
    pub items: ChipItems,
    /// A row or one of its actions was chosen. Runs after the menu closed.
    pub pick: ChipPick,
}

#[derive(Default)]
struct Chips(Vec<FooterChip>);
impl Global for Chips {}

/// Adds the chip, or replaces the one with the same id. Chips are drawn in
/// the order they were added.
pub(crate) fn register(chip: FooterChip, cx: &mut App) {
    let chips = &mut cx.default_global::<Chips>().0;
    match chips.iter_mut().find(|known| known.id == chip.id) {
        Some(known) => *known = chip,
        None => chips.push(chip),
    }
    super::refresh(cx);
}

pub(crate) fn unregister(id: &str, cx: &mut App) {
    cx.default_global::<Chips>().0.retain(|chip| chip.id != id);
    super::refresh(cx);
}

pub(crate) fn all(cx: &App) -> Vec<FooterChip> {
    cx.try_global::<Chips>()
        .map(|chips| chips.0.clone())
        .unwrap_or_default()
}

pub(crate) fn find(id: &str, cx: &App) -> Option<FooterChip> {
    cx.try_global::<Chips>()?
        .0
        .iter()
        .find(|chip| chip.id == id)
        .cloned()
}
