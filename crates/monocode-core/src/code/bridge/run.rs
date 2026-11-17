//! The run tools: what can be run in the session's checkout, starting it
//! without a terminal, and following it. Runs live in the shared registry,
//! so the user sees an agent's runs and an agent sees the user's.
use super::tools::{one_line, tail, untrusted, Call};
use crate::code::run::{
    builtin_tasks, resolve, RunConfig, RunContext, RunId, RunInfo, RunState, StartedBy,
};
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use std::time::{Duration, Instant, SystemTime};

/// How much output one answer carries.
const OUTPUT_BYTES: usize = 12_000;
const FINISHED_SHOWN: usize = 8;

fn command_line(spawn: &Spawn) -> String {
    let mut words = vec![spawn.command.clone()];
    words.extend(spawn.args.iter().cloned());
    one_line(
        &shlex::try_join(words.iter().map(String::as_str)).unwrap_or_else(|_| words.join(" ")),
        300,
    )
}

/// The context of a run: the checkout, and the file and test the agent named.
fn run_context(call: &Call) -> Result<RunContext, String> {
    let mut context = call.context.run_context();
    if let Some(file) = call.text("file")? {
        context.file = Some(call.file(file)?);
    }
    context.symbol = call.text("symbol")?.map(str::to_owned);
    Ok(context)
}

/// What `run_start` and `debug_start` are asked to start, resolved for the
/// session's checkout.
pub(super) fn target(call: &Call) -> Result<(Spawn, Toolchain), String> {
    let context = run_context(call)?;
    let toolchain = call.context.toolchain();
    let config = match (call.text("command")?, call.text("name")?) {
        (Some(_), Some(_)) => return Err("Give `name` or `command`, not both.".into()),
        (None, None) => {
            return Err("Give `name` (a configuration or task from run_list) or `command`.".into())
        }
        (Some(command), None) => {
            let cwd = match call.text("cwd")? {
                Some(cwd) => {
                    let folder = call.path(cwd)?;
                    if !folder.is_dir() {
                        return Err(format!("{cwd} is not a folder."));
                    }
                    folder.to_string_lossy().into_owned()
                }
                None => String::new(),
            };
            let mut env = String::new();
            for (key, value) in call.object("env")?.into_iter().flatten() {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("`env.{key}` must be a string."))?;
                if key.contains(['=', '\n']) || value.contains('\n') {
                    return Err(format!("`env.{key}` cannot be set this way."));
                }
                env.push_str(&format!("{key}={value}\n"));
            }
            RunConfig {
                command: command.to_owned(),
                cwd,
                env,
                ..RunConfig::default()
            }
        }
        (None, Some(name)) => {
            let runs = &call.context.runs;
            let tasks = builtin_tasks(&context, runs.test_runner);
            let named =
                |config: &&RunConfig| config.id == name || config.name.eq_ignore_ascii_case(name);
            match runs.configs.iter().find(named).or(tasks.iter().find(named)) {
                Some(config) => config.clone(),
                None if name.starts_with(crate::code::run::BUILTIN_PREFIX)
                    && context.file.is_none() =>
                {
                    return Err(format!(
                        "{name} is a task for a file: pass the `file` (and `symbol`) you gave \
                         run_list."
                    ))
                }
                None => {
                    return Err(format!(
                        "Nothing to run is called {name}. run_list shows what there is."
                    ))
                }
            }
        }
    };
    let spawn = resolve(&config, &context, &toolchain).map_err(|error| error.to_string())?;
    Ok((spawn, toolchain))
}

/// One line of `run_list`: what a configuration becomes here, or why it
/// cannot run.
fn described(
    call: &Call,
    config: &RunConfig,
    shown_as: &str,
    context: &RunContext,
    toolchain: &Toolchain,
) -> String {
    match resolve(config, context, toolchain) {
        Ok(spawn) => {
            let folder = match spawn.cwd == context.checkout {
                true => String::new(),
                false => format!("  (in {})", call.shown(&spawn.cwd)),
            };
            format!("  {shown_as}: {}{folder}", command_line(&spawn))
        }
        Err(error) => format!("  {shown_as}: cannot run now. {error}"),
    }
}

pub(super) fn list(call: &Call) -> Result<String, String> {
    let context = run_context(call)?;
    let toolchain = call.context.toolchain();
    let runs = &call.context.runs;
    let configured: Vec<String> = (runs.configs.iter())
        .map(|config| {
            let name = one_line(&config.label(), 120);
            described(call, config, &name, &context, &toolchain)
        })
        .collect();
    let tasks: Vec<String> = builtin_tasks(&context, runs.test_runner)
        .iter()
        .map(|task| {
            let name = format!("{} ({})", task.id, one_line(&task.label(), 80));
            described(call, task, &name, &context, &toolchain)
        })
        .collect();
    let mut text = String::new();
    match configured.is_empty() {
        true => text.push_str("The project has no run configurations of its own.\n"),
        false => text.push_str(&format!(
            "Run configurations (start one with run_start name=…):\n{}\n",
            configured.join("\n")
        )),
    }
    match (&context.file, tasks.is_empty()) {
        (Some(file), false) => text.push_str(&format!(
            "Tasks for {} (start one with run_start name=<id> and the same file):\n{}\n",
            call.shown(file),
            tasks.join("\n")
        )),
        (Some(file), true) => {
            text.push_str(&format!("No built-in tasks for {}.\n", call.shown(file)))
        }
        (None, _) => text.push_str("Pass `file` to see the tasks for a file or a test.\n"),
    }
    match toolchain.python() {
        Some(python) => text.push_str(&format!("Interpreter: {}", python.display())),
        None => text.push_str("Interpreter: no Python was found for this checkout."),
    }
    Ok(text)
}

fn who(call: &Call, started_by: StartedBy) -> &'static str {
    match started_by {
        StartedBy::User => "the user",
        StartedBy::Agent(session) if session == call.scope.session => "you",
        StartedBy::Agent(_) => "another agent",
    }
}

fn state_words(info: &RunInfo) -> String {
    match info.state {
        RunState::Running => "running".into(),
        RunState::Exited(Some(code)) => format!("exited with code {code}"),
        RunState::Exited(None) => "ended by a signal".into(),
        RunState::Stopped => "stopped".into(),
    }
}

fn seconds(info: &RunInfo) -> u64 {
    let end = info.ended_at.unwrap_or_else(SystemTime::now);
    end.duration_since(info.started_at)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A run of this session's project, by the id the agent gave.
fn run(call: &Call) -> Result<RunInfo, String> {
    let id = RunId(call.required_number("run")?);
    call.services
        .runs()
        .get(id)
        .filter(|info| call.owns(&info.checkout))
        .ok_or_else(|| {
            format!(
                "There is no run {} in this project. run_status lists them.",
                id.0
            )
        })
}

/// The state line and the output from `cursor` on, or the end of it.
fn report(call: &Call, info: &RunInfo, cursor: Option<u64>) -> String {
    let mut text = format!(
        "Run {} ({}): {}, {} s",
        info.id.0,
        one_line(&info.label, 120),
        state_words(info),
        seconds(info)
    );
    if let Some(url) = &info.url {
        text.push_str(&format!(", serving {url}"));
    }
    let Some(output) = call.services.runs().output(info.id, cursor.unwrap_or(0)) else {
        return text;
    };
    let mut next = output.next;
    let shown = match cursor {
        // Read forwards: the first part now, the rest with the next cursor.
        Some(_) if output.text.len() > OUTPUT_BYTES => {
            let mut cut = OUTPUT_BYTES;
            while !output.text.is_char_boundary(cut) {
                cut -= 1;
            }
            next -= (output.text.len() - cut) as u64;
            text.push_str("\nMore output follows: call again with the cursor.");
            output.text[..cut].to_owned()
        }
        Some(_) => {
            if output.truncated {
                text.push_str("\nOutput before this is no longer kept.");
            }
            output.text
        }
        None => tail(&output.text, OUTPUT_BYTES),
    };
    text.push_str(&format!("\ncursor: {next}\n"));
    match shown.is_empty() {
        true => text.push_str("No new output."),
        false => text.push_str(&untrusted("Program output", &shown)),
    }
    text
}

pub(super) fn start(call: &Call) -> Result<String, String> {
    let wait = call.wait("wait_s", 0, super::MAX_WAIT)?;
    let (spawn, toolchain) = target(call)?;
    let line = command_line(&spawn);
    let runs = call.services.runs();
    let id = runs.start_headless(
        spawn,
        &call.context.checkout,
        StartedBy::Agent(call.scope.session),
        &toolchain,
    )?;
    let deadline = Instant::now() + wait;
    let info = loop {
        let Some(info) = runs.get(id) else {
            return Err("The run was removed.".into());
        };
        if !info.state.is_running() || Instant::now() >= deadline {
            break info;
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let mut text = format!("Started run {}: {line}\n", id.0);
    if wait.is_zero() {
        text.push_str(&format!(
            "It is running (pid {}). Read it with run_output run={}.",
            info.pid.map_or("?".into(), |pid| pid.to_string()),
            id.0
        ));
        return Ok(text);
    }
    text.push_str(&report(call, &info, None));
    Ok(text)
}

pub(super) fn output(call: &Call) -> Result<String, String> {
    let info = run(call)?;
    Ok(report(call, &info, call.number("cursor")?))
}

pub(super) fn stop(call: &Call) -> Result<String, String> {
    let info = run(call)?;
    if info.started_by != StartedBy::Agent(call.scope.session) {
        return Err(format!(
            "Run {} was started by {}; it is theirs to stop.",
            info.id.0,
            who(call, info.started_by)
        ));
    }
    if !info.state.is_running() {
        return Ok(format!(
            "Run {} had already {}.",
            info.id.0,
            state_words(&info)
        ));
    }
    let runs = call.services.runs();
    runs.stop(info.id);
    // A headless run gets a moment to end by itself before it is killed.
    let deadline = Instant::now() + Duration::from_secs(4);
    let info = loop {
        let now = runs.get(info.id).unwrap_or_else(|| info.clone());
        if !now.state.is_running() || Instant::now() >= deadline {
            break now;
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    match info.state.is_running() {
        true => Ok(format!(
            "Run {} was asked to stop and has not ended yet.",
            info.id.0
        )),
        false => Ok(report(call, &info, None)),
    }
}

pub(super) fn status(call: &Call) -> Result<String, String> {
    let mine: Vec<RunInfo> = (call.services.runs().list().into_iter())
        .filter(|info| call.owns(&info.checkout))
        .collect();
    let line = |info: &RunInfo| {
        let mut text = format!(
            "  {}  {}  {}, {} s, started by {}",
            info.id.0,
            command_line(&info.spawn),
            state_words(info),
            seconds(info),
            who(call, info.started_by)
        );
        if !info.headless {
            text.push_str(", in a terminal");
        }
        if let Some(url) = &info.url {
            text.push_str(&format!(", serving {url}"));
        }
        text
    };
    let running: Vec<String> = (mine.iter())
        .filter(|info| info.state.is_running())
        .map(line)
        .collect();
    let finished: Vec<String> = (mine.iter().rev())
        .filter(|info| !info.state.is_running())
        .take(FINISHED_SHOWN)
        .map(line)
        .collect();
    let mut text = match running.is_empty() {
        true => "Nothing is running in this project.".to_owned(),
        false => format!("Running:\n{}", running.join("\n")),
    };
    if !finished.is_empty() {
        text.push_str(&format!("\nEnded, newest first:\n{}", finished.join("\n")));
    }
    Ok(text)
}
