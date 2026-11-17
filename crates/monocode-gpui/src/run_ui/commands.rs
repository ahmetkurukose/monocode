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
use gpui::App;
use monocode_core::{code::services::services, files::path_key};
use std::{path::Path, rc::Rc};

pub(super) fn init(cx: &mut App) {
    commands::register_commands("run", list, cx);
}

fn list(scope: &Scope, cx: &App) -> Vec<Command> {
    if scope.project.is_empty() || scope.project.starts_with("remote://") {
        return Vec::new();
    }
    let checkout = Checkout {
        project: scope.project.clone(),
        folder: scope.folder.clone(),
    };
    // The focused file counts when it is one of this checkout's.
    let focused = context::focused_file(cx)
        .filter(|file| path_key(&file.checkout.folder) == path_key(&checkout.folder));
    let context = match &focused {
        Some(file) => targets::editor_context(Path::new(&file.path), &checkout, None, cx),
        None => targets::bare_context(&checkout),
    };
    let choices = targets::choices(&checkout, &context, cx);
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
