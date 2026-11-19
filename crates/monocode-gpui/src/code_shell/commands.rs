//! What a feature adds to quick open: commands for the `>` palette, and
//! modes of its own behind a prefix character (`@` for symbols) whose rows
//! come from the feature, asynchronously. `quick_open.rs` reads these lists
//! and knows nothing of who filled them.
use gpui::{App, Global, SharedString, Task, Window};
use std::rc::Rc;

/// Where quick open was opened: the project in front and the checkout its
/// files are listed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scope {
    pub project: String,
    pub folder: String,
}

/// What choosing a row does. It runs after quick open has closed and the
/// shell's update is over, so it may call anything in `code_shell`.
pub(crate) type Action = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct Command {
    pub title: SharedString,
    /// Shown under the title: a shortcut, or what the command acts on.
    pub subtitle: SharedString,
    pub run: Action,
}

/// A row of a prefix mode: plain data, so a search can build its rows on
/// another thread.
#[derive(Clone, Default)]
pub(crate) struct Item {
    pub title: SharedString,
    pub detail: SharedString,
    /// The file the row opens, absolute, at `line` and `column` (1-based;
    /// zero opens it at the top).
    pub path: Option<String>,
    pub line: u32,
    pub column: u32,
    /// Shown in place of the file's icon.
    pub icon: Option<&'static str>,
}

type CommandSource = Rc<dyn Fn(&Scope, &App) -> Vec<Command>>;
type Applies = Rc<dyn Fn(&App) -> bool>;
type Search = Rc<dyn Fn(String, Scope, &mut App) -> Task<Vec<Item>>>;

#[derive(Clone)]
pub(crate) struct Mode {
    /// Fixed and unique, such as `"code-symbols"`.
    pub id: &'static str,
    /// Typed first in the search box to enter the mode. `>` is taken.
    pub prefix: char,
    /// Shown while the mode has nothing to list: "Type to search symbols".
    pub hint: &'static str,
    /// Whether the mode has something to search right now. While it has
    /// not, its prefix is an ordinary character of the query, so `@types`
    /// still finds the folder.
    pub applies: Applies,
    /// The rows for the text after the prefix. Called on the UI thread a
    /// moment after typing stops; the work belongs in the task. A newer
    /// query drops the task of the one before.
    pub search: Search,
}

#[derive(Default)]
struct Registry {
    sources: Vec<(&'static str, CommandSource)>,
    modes: Vec<Mode>,
}
impl Global for Registry {}

/// Adds commands to the `>` palette. `source` is asked each time quick open
/// ranks its rows, so it can offer what exists right now (a project's run
/// configurations); it must be cheap. Registering `id` again replaces it.
pub(crate) fn register_commands(
    id: &'static str,
    source: impl Fn(&Scope, &App) -> Vec<Command> + 'static,
    cx: &mut App,
) {
    let registry = cx.default_global::<Registry>();
    registry.sources.retain(|(known, _)| *known != id);
    registry.sources.push((id, Rc::new(source)));
}

/// Adds a prefix mode, replacing one with the same id.
pub(crate) fn register_mode(mode: Mode, cx: &mut App) {
    let registry = cx.default_global::<Registry>();
    registry.modes.retain(|known| known.id != mode.id);
    registry.modes.push(mode);
}

pub(crate) fn unregister(id: &str, cx: &mut App) {
    let registry = cx.default_global::<Registry>();
    registry.sources.retain(|(known, _)| *known != id);
    registry.modes.retain(|known| known.id != id);
}

/// Every contributed command for `scope`.
pub(crate) fn commands(scope: &Scope, cx: &App) -> Vec<Command> {
    let Some(registry) = cx.try_global::<Registry>() else {
        return Vec::new();
    };
    (registry.sources.iter())
        .flat_map(|(_, source)| source(scope, cx))
        .collect()
}

/// The mode `query` is in, with what follows its prefix.
pub(crate) fn mode_for(query: &str, cx: &App) -> Option<(Mode, String)> {
    let prefix = query.chars().next()?;
    let mode = (cx.try_global::<Registry>()?.modes.iter())
        .find(|mode| mode.prefix == prefix && (mode.applies)(cx))?;
    Some((mode.clone(), query[prefix.len_utf8()..].trim().to_owned()))
}
