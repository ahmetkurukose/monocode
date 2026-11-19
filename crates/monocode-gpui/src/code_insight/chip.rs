//! The footer's language servers chip. It reflects the servers of the
//! checkout in front: absent while there is none, quiet when they are
//! ready, busy while one starts or indexes, and a warning when a tool is
//! missing or a server failed. Its list names each server with its root,
//! state, open documents and memory, and lets one be restarted or stopped;
//! for a missing tool it says what was looked for and what to install.
use super::settings::PAGE;
use crate::code_shell::{
    chips::{self, ChipItem, ChipState, FooterChip, Tone},
    context, settings as pages,
};
use gpui::{App, ClipboardItem, SharedString, Window};
use monocode_core::code::lsp::{ServerId, ServerState, ServerStatus};
use std::{path::Path, rc::Rc};

pub(super) const CHIP: &str = "code-servers";

pub(super) fn register(cx: &mut App) {
    chips::register(
        FooterChip {
            id: CHIP,
            label: "Language servers",
            state: Rc::new(|cx| state(&statuses(cx))),
            items: Rc::new(|cx| {
                let memory =
                    |pid: u32| super::sync(cx).and_then(|sync| sync.read(cx).memory_of(pid));
                items(&statuses(cx), memory)
            }),
            pick: Rc::new(pick),
        },
        cx,
    );
}

/// The servers of the checkout in front and of the file in front, which
/// may be another's; every server when nothing says where the user is.
fn statuses(cx: &App) -> Vec<ServerStatus> {
    let Some(manager) = super::manager(cx) else {
        return Vec::new();
    };
    let mut all = manager.statuses();
    let folders: Vec<String> = (context::focused_checkout(cx).into_iter())
        .chain(context::focused_file(cx).map(|file| file.checkout))
        .map(|checkout| checkout.folder)
        .collect();
    if !folders.is_empty() {
        all.retain(|status| folders.iter().any(|folder| status.root.starts_with(folder)));
    }
    all
}

fn state_word(state: &ServerState) -> SharedString {
    match state {
        ServerState::Starting => "Starting".into(),
        ServerState::Ready => "Ready".into(),
        ServerState::Indexing { title } => match title {
            Some(title) if !title.is_empty() => title.clone().into(),
            _ => "Indexing".into(),
        },
        ServerState::Failed { .. } => "Failed".into(),
        ServerState::Stopped => "Stopped".into(),
        ServerState::ToolNotFound { .. } => "Not installed".into(),
    }
}

fn tone(state: &ServerState) -> Tone {
    match state {
        ServerState::Starting | ServerState::Indexing { .. } => Tone::Busy,
        ServerState::Ready | ServerState::Stopped => Tone::Quiet,
        ServerState::Failed { .. } | ServerState::ToolNotFound { .. } => Tone::Warning,
    }
}

/// What the chip shows for these servers; `None` hides it.
pub(super) fn state(statuses: &[ServerStatus]) -> Option<ChipState> {
    let first = statuses.first()?;
    let of = |wanted: Tone| statuses.iter().find(|status| tone(&status.state) == wanted);
    let (tone, text): (Tone, SharedString) = if let Some(status) = of(Tone::Warning) {
        let what = match status.state {
            ServerState::ToolNotFound { .. } => "not installed",
            _ => "failed",
        };
        (Tone::Warning, format!("{} {what}", status.name).into())
    } else if let Some(status) = of(Tone::Busy) {
        let doing = match &status.state {
            ServerState::Starting => "starting".into(),
            other => state_word(other).to_lowercase(),
        };
        (Tone::Busy, format!("{}: {doing}", status.name).into())
    } else if statuses.len() == 1 {
        (Tone::Quiet, first.name.clone().into())
    } else {
        (Tone::Quiet, format!("{} servers", statuses.len()).into())
    };
    let tooltip = (statuses.iter())
        .map(|status| format!("{}: {}", status.name, state_word(&status.state)))
        .collect::<Vec<_>>()
        .join("\n");
    Some(ChipState {
        icon: "code",
        text,
        tone,
        tooltip: format!("Language servers\n{tooltip}").into(),
    })
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1024 * 1024))
}

/// A root as it is read: from the home folder where it is under it.
fn shown_root(root: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.and_then(|home| root.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => root.display().to_string(),
    }
}

/// The chip's list: a row per server, and under a missing or failed one a
/// row that copies what to do about it.
pub(super) fn items(
    statuses: &[ServerStatus],
    memory: impl Fn(u32) -> Option<u64>,
) -> Vec<ChipItem> {
    let mut rows = Vec::new();
    for status in statuses {
        let id = status.id.0;
        let mut detail = vec![state_word(&status.state).to_string()];
        if status.open_documents > 0 {
            detail.push(match status.open_documents {
                1 => "1 file".into(),
                n => format!("{n} files"),
            });
        }
        if let Some(bytes) = status.pid.and_then(&memory) {
            detail.push(megabytes(bytes));
        }
        let action = |verb: &str, label: &str| -> (SharedString, SharedString) {
            (format!("{verb}:{id}").into(), label.to_owned().into())
        };
        let actions = match &status.state {
            ServerState::Starting | ServerState::Ready | ServerState::Indexing { .. } => {
                vec![action("restart", "Restart"), action("stop", "Stop")]
            }
            ServerState::Stopped => vec![action("restart", "Start")],
            ServerState::Failed { .. } => vec![
                action("restart", "Restart"),
                action("copy", "Copy the error"),
            ],
            ServerState::ToolNotFound { .. } => vec![
                action("restart", "Look again"),
                action("copy", "Copy what to install"),
                action("settings", "Set its path…"),
            ],
        };
        rows.push(ChipItem {
            id: format!("server:{id}").into(),
            title: format!("{}  {}", status.name, shown_root(&status.root)).into(),
            detail: Some(detail.join(" · ").into()),
            icon: Some("code"),
            tone: tone(&status.state),
            actions,
        });
        let note = match &status.state {
            ServerState::ToolNotFound { looked_for, hint } => {
                Some(format!("Looked for {}. {hint}", looked_for.join(", ")))
            }
            ServerState::Failed { message, .. } => Some(message.clone()),
            _ => None,
        };
        if let Some(note) = note {
            rows.push(ChipItem {
                id: format!("copy:{id}").into(),
                title: note.into(),
                detail: Some("Copy".into()),
                icon: None,
                tone: Tone::Warning,
                actions: Vec::new(),
            });
        }
    }
    rows
}

/// What "copy" puts on the clipboard for a server: the install hint with
/// the names looked for, or the failure with the end of the server's log.
fn copied(status: &ServerStatus) -> Option<String> {
    match &status.state {
        ServerState::ToolNotFound { looked_for, hint } => Some(format!(
            "{} was not found (looked for: {}).\n{hint}",
            status.server,
            looked_for.join(", ")
        )),
        ServerState::Failed { message, log_tail } => Some(match log_tail.trim().is_empty() {
            true => format!("{message}\n{}", status.command),
            false => format!("{message}\n{}\n\n{}", status.command, log_tail.trim_end()),
        }),
        _ => None,
    }
}

/// A row or one of its actions was chosen: `verb:server id`.
pub(super) fn pick(id: &str, window: &mut Window, cx: &mut App) {
    let Some((verb, server)) = id.split_once(':') else {
        return;
    };
    let (Some(manager), Ok(server)) = (super::manager(cx), server.parse::<u64>()) else {
        return;
    };
    let server = ServerId(server);
    let status = (manager.statuses().into_iter()).find(|status| status.id == server);
    match (verb, status) {
        ("restart", Some(status)) => match status.state {
            // The tool may have been installed, or a virtual environment
            // made, since the checkout was looked at: look again.
            ServerState::ToolNotFound { .. } | ServerState::Failed { .. } => {
                if let Some(sync) = super::sync(cx) {
                    sync.update(cx, |sync, cx| sync.reload_checkout(&status.root, cx));
                }
            }
            _ => manager.restart_server(server),
        },
        ("stop", Some(_)) => manager.stop_server(server),
        ("copy", Some(status)) => {
            if let Some(text) = copied(&status) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        ("settings", Some(status)) => {
            let project = super::sync(cx)
                .and_then(|sync| sync.read(cx).project_of(&status.root))
                .map(|project| project.to_string_lossy().into_owned())
                .or_else(|| context::focused_checkout(cx).map(|focus| focus.project));
            if let Some(project) = project {
                pages::open(&project, PAGE, window, cx);
            }
        }
        _ => {}
    }
    crate::code_shell::refresh(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: u64, name: &str, state: ServerState) -> ServerStatus {
        ServerStatus {
            id: ServerId(id),
            server: name.into(),
            name: name.into(),
            root: "/work/app".into(),
            state,
            pid: Some(40 + id as u32),
            command: format!("/venv/bin/{name} --stdio"),
            open_documents: 2,
        }
    }

    #[test]
    fn the_chip_is_quiet_when_ready_busy_while_working_and_warns_of_trouble() {
        assert_eq!(state(&[]), None);
        let ready = status(1, "basedpyright", ServerState::Ready);
        let shown = state(std::slice::from_ref(&ready)).unwrap();
        assert_eq!(
            (shown.tone, shown.text.as_ref()),
            (Tone::Quiet, "basedpyright")
        );
        let indexing = status(
            2,
            "ruff",
            ServerState::Indexing {
                title: Some("Indexing".into()),
            },
        );
        let shown = state(&[ready.clone(), indexing.clone()]).unwrap();
        assert_eq!(
            (shown.tone, shown.text.as_ref()),
            (Tone::Busy, "ruff: indexing")
        );
        let starting = status(3, "gopls", ServerState::Starting);
        let shown = state(&[starting]).unwrap();
        assert_eq!(
            (shown.tone, shown.text.as_ref()),
            (Tone::Busy, "gopls: starting")
        );
        let missing = status(
            4,
            "ruff",
            ServerState::ToolNotFound {
                looked_for: vec!["ruff".into()],
                hint: "Install it with: pip install ruff".into(),
            },
        );
        // Trouble outranks work in progress.
        let shown = state(&[indexing, missing.clone()]).unwrap();
        assert_eq!(
            (shown.tone, shown.text.as_ref()),
            (Tone::Warning, "ruff not installed")
        );
        let failed = status(
            5,
            "basedpyright",
            ServerState::Failed {
                message: "basedpyright exited while starting.".into(),
                log_tail: "Error: node not found\n".into(),
            },
        );
        let shown = state(std::slice::from_ref(&failed)).unwrap();
        assert_eq!(
            (shown.tone, shown.text.as_ref()),
            (Tone::Warning, "basedpyright failed")
        );
        assert!(shown.tooltip.contains("basedpyright: Failed"));

        // The list: state, documents and memory, with what can be done.
        let rows = items(&[ready, missing.clone(), failed.clone()], |pid| {
            (pid == 41).then_some(300 * 1024 * 1024)
        });
        assert_eq!(rows[0].title.as_ref(), "basedpyright  /work/app");
        assert_eq!(rows[0].detail.as_deref(), Some("Ready · 2 files · 300 MB"));
        let verbs = |row: &ChipItem| -> Vec<String> {
            (row.actions.iter()).map(|(id, _)| id.to_string()).collect()
        };
        assert_eq!(verbs(&rows[0]), ["restart:1", "stop:1"]);
        assert_eq!(verbs(&rows[1]), ["restart:4", "copy:4", "settings:4"]);
        assert_eq!(rows[2].id.as_ref(), "copy:4");
        assert_eq!(
            rows[2].title.as_ref(),
            "Looked for ruff. Install it with: pip install ruff"
        );
        assert_eq!(
            rows[4].title.as_ref(),
            "basedpyright exited while starting."
        );
        assert_eq!(
            copied(&missing).unwrap(),
            "ruff was not found (looked for: ruff).\nInstall it with: pip install ruff"
        );
        assert!(copied(&failed).unwrap().ends_with("Error: node not found"));
    }
}
