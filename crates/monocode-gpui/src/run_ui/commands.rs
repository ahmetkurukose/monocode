//! Run's commands in quick open: every configuration and built-in task
//! offered where quick open was opened, the last run again, a stop for each
//! thing that is running, and the way to the configurations.
use super::{
    chip,
    header::{self, PAGE, RERUN_KEY},
    start, targets,
};
use crate::code_shell::{
    commands::{self, Command, Scope},
    context::{self, Checkout},
    settings,
};
use gpui::{App, Global};
use monocode_core::{
    code::{
        run::{ProjectRuns, RunContext},
        services::services,
    },
    files::path_key,
};
use std::{cell::RefCell, path::Path, rc::Rc, time::Instant};

pub(super) fn init(cx: &mut App) {
    cx.set_global(Cache::default());
    commands::register_commands("run", list, cx);
}

/// Everything what is offered depends on. Quick open asks for the commands
/// on every keystroke, and none of this changes while it is typed in.
#[derive(PartialEq)]
struct Place {
    checkout: Checkout,
    /// The focused file: its path and pane, the caret, the selection and
    /// the hub's version of its text.
    file: Option<targets::FileKey>,
    runs: ProjectRuns,
}

struct Offered {
    place: Place,
    at: Instant,
    context: RunContext,
    choices: targets::Choices,
}

/// What was last offered, kept while the place it was built for stands and
/// for as long as a toolchain is trusted.
#[derive(Default)]
struct Cache {
    offered: RefCell<Option<Offered>>,
    #[cfg(test)]
    built: std::cell::Cell<usize>,
}
impl Global for Cache {}

/// How often the offered runs were worked out, for the tests.
#[cfg(test)]
pub(super) fn times_built(cx: &App) -> usize {
    cx.global::<Cache>().built.get()
}

/// The run context and the choices for quick open in `checkout`. Building
/// them reads the disk (the interpreter, the packages a file is in), so
/// they are kept between keystrokes.
fn offered(checkout: &Checkout, cx: &App) -> (RunContext, targets::Choices) {
    // The focused file counts when it is one of this checkout's.
    let focused = context::focused_file(cx)
        .filter(|file| path_key(&file.checkout.folder) == path_key(&checkout.folder));
    let file = focused
        .as_ref()
        .and_then(|file| targets::file_key(file, cx));
    let place = Place {
        checkout: checkout.clone(),
        file,
        runs: settings::runs(&checkout.project, cx),
    };
    let cache = cx.try_global::<Cache>();
    if let Some(cache) = cache
        && let Some(known) = cache.offered.borrow().as_ref()
        && known.place == place
        && known.at.elapsed() < targets::TOOLCHAIN_AGE
    {
        return (known.context.clone(), known.choices.clone());
    }
    let context = match &focused {
        Some(file) => targets::editor_context(Path::new(&file.path), checkout, None, cx),
        None => targets::bare_context(checkout),
    };
    let choices = targets::choices(checkout, &context, cx);
    if let Some(cache) = cache {
        #[cfg(test)]
        cache.built.set(cache.built.get() + 1);
        *cache.offered.borrow_mut() = Some(Offered {
            place,
            at: Instant::now(),
            context: context.clone(),
            choices: choices.clone(),
        });
    }
    (context, choices)
}

fn list(scope: &Scope, cx: &App) -> Vec<Command> {
    if scope.project.is_empty() || scope.project.starts_with("remote://") {
        return Vec::new();
    }
    let checkout = Checkout {
        project: scope.project.clone(),
        folder: scope.folder.clone(),
    };
    let (context, choices) = offered(&checkout, cx);
    let mut list = Vec::new();
    let user = choices.user.len();
    for (n, config) in choices.all().enumerate() {
        let (config, context, checkout) = (config.clone(), context.clone(), checkout.clone());
        list.push(Command {
            title: format!("Run: {}", config.label()).into(),
            subtitle: if n < user {
                "Run configuration"
            } else {
                "This file"
            }
            .into(),
            run: Rc::new(move |_, cx| {
                start::start(config.clone(), context.clone(), checkout.clone(), cx);
            }),
        });
    }
    if start::has_last(cx) {
        list.push(Command {
            title: "Run: Rerun last".into(),
            subtitle: header::shown_key(RERUN_KEY).into(),
            run: Rc::new(|_, cx| {
                start::rerun_last(cx);
            }),
        });
    }
    let project = path_key(&scope.project);
    for info in services().runs().list() {
        if !info.state.is_running() || path_key(&chip::checkout_of(&info, cx).project) != project {
            continue;
        }
        let id = info.id;
        list.push(Command {
            title: format!("Run: Stop {}", info.label).into(),
            subtitle: "Running".into(),
            run: Rc::new(move |_, _| start::stop(id)),
        });
    }
    let project = scope.project.clone();
    list.push(Command {
        title: "Run: Edit configurations".into(),
        subtitle: "Project settings".into(),
        run: Rc::new(move |window, cx| {
            settings::open(&project, PAGE, window, cx);
        }),
    });
    list
}
