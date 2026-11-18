# Code: run, code insight and debug

Status: in progress on branch `AK/ide-features` (started 2026-11-18). This file is the
contract the parts are built against; each part's section is filled in by whoever builds it.

## 1. Goal

Monocode is an agent development environment, not an IDE. This adds the three things that
most often send its user to an IDE, and no more:

1. **Run**: run configurations per project, "run this file / this test", output in the dock.
2. **Code insight**: syntax and type errors, go to definition, find usages, hover,
   completion and signature help, from language servers.
3. **Debug**: breakpoints, stepping, the stack, variables and a console, over the Debug
   Adapter Protocol.

Each is one headless service in `monocode-core` with two faces: the panes, and MCP tools so
an agent can run, inspect and debug the same way the user does.

### Decided, do not reopen

- **Own implementation.** Zed was studied as a design reference; no Zed crates or code.
- **Python first** (basedpyright + ruff, debugpy). TypeScript/JavaScript, Go and Rust are
  registered where it costs nothing, and are otherwise later work.
- **Nothing is downloaded.** A tool is looked for at the path the user set, then in the
  checkout's virtual environment, then on `PATH` (`code::toolchain::Toolchain`). A missing
  tool is reported by name with what to install.
- **The interpreter comes from the project's terminal setup** (its venv), with `.venv` /
  `venv` found even when terminals do not activate one.
- **No telemetry, no network calls** from any of this.
- **Out of scope:** VS Code `tasks.json` / `launch.json` import, any external-IDE bridge,
  rename, code actions, multiple cursors, remote projects (local checkouts only for now).
- **No new window region.** Everything goes into surfaces that exist: the file pane's header
  and gutter, the terminal dock, the right panel, the footer, quick open, project settings.
- **Fork hygiene.** New code lives in its own modules; `shell.rs`, `file_editor.rs` and
  `input/code.rs` get thin hooks only, as with the Database view.

## 2. Layout

```
crates/monocode-core/src/code/
  wire.rs        Content-Length framing shared by LSP and DAP
  toolchain.rs   a checkout's venv, interpreter and tool lookup
  spawn.rs       one resolved process to start
  run/           run configurations, variables, built-in Python tasks
  lsp/           language server client, registry and manager
  dap/           debug adapter client, session model, breakpoints
crates/monocode-gpui/src/
  code_insight/  diagnostics, navigation, hover, completion in the file editor
  run_ui/        header and gutter run buttons, dock run tabs, footer chip, settings
  debug_ui/      breakpoints in the gutter, the debug dock tab
```

Core conventions: plain threads and channels as in `browser_bridge.rs`; events reach the
UI over `async_channel`; child processes go through `process::configure_child` and are
killed when their owner is dropped. Every part has unit tests against a scripted fake
peer. Tests that need real tools read `MONOCODE_TEST_PYTHON` (a venv interpreter with
basedpyright, ruff, debugpy and pytest installed) and pass trivially when it is unset.

## 3. Run

Everything is in `monocode_core::code::run` (re-exported from its `mod.rs`); no UI, no
terminal. The dock executes the resolved `Spawn`; MCP tools use the same calls.

**Configuration and storage.** `RunConfig { id, name, command, args, cwd, env }`, all
strings as typed, camelCase in JSON. `command` (+ `args`, appended) is a command line split
by shell rules (`shlex`); no shell runs it, so pipes need `sh -c '…'`. `cwd` is empty (the
checkout), relative to the checkout, or absolute. `env` is `KEY=VALUE` lines
(`terminal_setup::parse_environment`). A project keeps `ProjectRuns { configs, last_used,
test_runner }` with `add` (assigns the id) / `update` / `remove` / `move_to` / `get` /
`mark_used` / `last_used` / `normalized` / `is_empty`. Configurations are defined **per
project and resolved per checkout**. They are saved beside the terminal setup, in the
workspace state: `Workspace::project_runs: HashMap<project_path, ProjectRuns>`, normalized
on restore, an empty entry dropped.

**Variables.** `$FILE`, `$RELATIVE_FILE`, `$FILE_DIR`, `$FILE_NAME`, `$FILE_STEM`,
`$CHECKOUT`, `$PROJECT`, `$SYMBOL`, `$ROW`, `$COLUMN`, `$SELECTED_TEXT`, `$PYTHON`,
`$MODULE`; also as `${NAME}`; `$$` is a dollar sign. They are filled in after splitting, so
a path with spaces is one argument, quoted or not, and they work in `cwd` and `env` values
too. A dollar sign before any other name (`$HOME`, `$1`) is left as typed. `RunContext {
checkout, project, file, symbol, row, column, selected_text }` carries what the caller
knows (`row`/`column` from 1, as shown); `$PYTHON` comes from the `Toolchain`, `$MODULE`
from `module_path(file, checkout)`: the file's folders are followed upwards while they hold
an `__init__.py`, the checkout being the limit.

**Resolution.** `resolve(&RunConfig, &RunContext, &Toolchain) -> Result<Spawn, Unresolved>`.
`Unresolved::Needs(Variable)` means the context lacks a value: the configuration is **not
offered** (`offered(&ProjectRuns, &RunContext, &Toolchain)` filters the list); `Empty` and
`Unbalanced` are mistakes to show in settings (`Display` gives the sentence). `Spawn::env`
holds only the configuration's variables; the starter applies `Toolchain::environment`
first. A program that is exactly `python` or `python3` becomes the checkout's interpreter.

**Built-in tasks.** `builtin_tasks(&RunContext, TestRunner) -> Vec<RunConfig>` for the
focused file, most specific first; `ProjectRuns::test_runner` is `Pytest` (default) or
`Unittest`. Tasks are ordinary `RunConfig`s with ids `builtin:<language>:<kind>:<target>`
that name the file from the checkout and the interpreter as `$PYTHON`, so `mark_used` can
store one and it repeats in any checkout.

| Target | Command | cwd |
|---|---|---|
| Python file | `$PYTHON <rel>` | checkout |
| … in a package | `$PYTHON -m <module>` | module root |
| pytest file / class / test | `$PYTHON -m pytest <rel>[::Class][::test]` | checkout |
| unittest file / class / test | `$PYTHON -m unittest <module>[.Class[.test]]` | module root |
| Go | `go run .`, `go test .`, `go test -run '^Name$' .` | file's folder |
| Rust | `cargo run`, `cargo test [mods::name]` | file's folder |
| JS / TS | `node <rel>` / `npx tsx <rel>` | checkout |

A Python test file is `test_*.py` or `*_test.py` (`is_python_test_file`); in one, a
`context.symbol` naming a test puts that test first. unittest cannot address a function
outside a class, so that marker has no task under it.

**Runnables (gutter markers).** The UI detects, core maps:
`runnable_tasks(&Runnable, &RunContext, TestRunner) -> Vec<RunConfig>` with `context.file`
the marker's file; the first task is the click, the rest the menu. `Runnable { kind, name,
class_name, row }`; `class_name` is the enclosing classes outermost first joined with `::`
(Rust: enclosing modules); `row` is from 0 and only places the marker. Detection rules:

- `PythonTestFunction`: only in a test file; a `def`/`async def` (decorated or not) whose
  name starts with `test`, either at module level or directly in a test class.
- `PythonTestClass`: only in a test file; a class whose name starts with `Test`, or with a
  base whose text ends in `TestCase`; at module level or nested in another test class. The
  editor leaves out a `Test…` class that has an `__init__` and no `TestCase` base, as
  pytest does.
- `PythonMain`: any `.py` file; a module-level `if` whose condition compares `__name__`
  with the string `__main__` (either quote, either order). `name` is the file name.
- `GoTest`: `func TestXxx(…)` in `*_test.go`. `GoMain`: `func main()` in `package main`.
- `RustTest`: a function with a `#[test]` (or `#[tokio::test]`) attribute. `RustMain`:
  a top-level `fn main()`.

**Running registry.** `registry()` is the app's `Registry` (cloneable, any thread).
A terminal run: `register(spawn, checkout, StartedBy::User | Agent(session))` → `RunId`,
then the terminal reports `set_pid`, `append_output` (raw text; escape codes are stripped)
and `finish(code)`. `stop(id)` only marks the run and emits `RunEvent::StopRequested`; the
terminal ends the process and calls `finish`, which then yields `RunState::Stopped`
instead of `Exited(code)`. `subscribe()` gives an `async_channel::Receiver<RunEvent>`
(`Started`, `Output`, `Changed`, `StopRequested`, `Removed`); `get` / `list` return
`RunInfo` snapshots (state, pid, times, `headless`, `stop_requested`, and `url`: the first
local address the output announced). `output(id, from)` returns the plain-text tail after
an offset (about 256 KiB kept) with the offset to continue from. The last 40 finished runs
are kept; `remove` forgets one.

**Headless runs.** `start_headless(spawn, checkout, started_by, &toolchain)` starts the
process itself (own process group, no stdin, stdout and stderr into the tail, app
environment + toolchain's + the spawn's, `PYTHONUNBUFFERED=1`). `stop` sends the group
SIGTERM, then SIGKILL after 2 s; `shutdown()` kills all of them at app quit.

**Ports.** `local_urls(text)` / `first_local_url(text)` find `http://localhost:5173/`,
`127.0.0.1:8000`, "Listening on port 3000" (as `http://localhost:3000`); only this
machine's addresses, `0.0.0.0` mapped to `127.0.0.1`.

## 4. Code insight

`code::lsp` is a headless Language Server Protocol client and the manager that runs the
servers. `lsp::Manager` is the only way in, for the editor and for the agents' tools alike.

**Servers** (`registry.rs`; language is decided by file extension, order is priority):

| Language | Server id | Looked for, in order | Asked for |
| --- | --- | --- | --- |
| Python | `basedpyright` | `basedpyright-langserver --stdio`, `pyright-langserver --stdio` | everything |
| Python | `ruff` | `ruff server` | diagnostics only |
| TS/JS (+x) | `typescript` | `vtsls --stdio`, `typescript-language-server --stdio` | everything |
| Go | `gopls` | `gopls` | everything |
| Rust | `rust-analyzer` | `rust-analyzer` | everything |

(based)pyright gets the interpreter as `python.pythonPath` and `diagnosticMode:
openFilesOnly` through `workspace/configuration`, so a monorepo is not analysed whole.
A path the user set for a server id (`ManagerOptions::tool_paths`) is used or nothing is.
Only Python has been run against real servers; the others are registered and untested.

**Root and identity.** The root is the nearest folder at or above the file, inside the
checkout, holding one of the server's markers (`pyproject.toml`, `setup.py`,
`package.json`, `go.mod`, `Cargo.toml`, ...), else the checkout. One process per (root,
server id, toolchain). A file outside the checkout (a library reached by go to
definition) joins the server that pointed at it, else the checkout's most recently used
server of that kind; it never starts a server rooted outside.

**Lifecycle and memory.** Servers start on the first `open`, off the caller's thread;
documents opened meanwhile are sent when the handshake is done. Each server counts its
open documents. With none left it is stopped after `idle_period` (5 minutes), and at most
`max_servers` (6) are alive: idle ones are stopped least recently used first; servers with
documents are never stopped for the cap, so it can be exceeded. Each server leads its own
process group and the group is killed, since basedpyright is a launcher that runs node:
memory for `ServerStatus::pid` is the sum over that group. A failed start is retried on
the next `open`, three times, then only by `restart_server`.

**API** (`Manager` is `Clone + Send + Sync`; paths are absolute; offsets are UTF-8 bytes):

- Never wait (fine on the UI thread): `open(path, text, &DocumentContext)`,
  `change(path, text)` (whole text), `close(path)`, `is_open`, `diagnostics(path)`,
  `triggers(path)`, `statuses()`, `stop_server(id)`, `restart_server(id)`, `stop_all()`,
  `subscribe()`, `sweep()`. `open` is counted: every `open` needs its `close`, and a
  second opener does not replace the text.
- Wait for a server, at most `request_timeout` (10 s) for it to start plus the same for
  its answer, so call them on a background thread, each with a `CancelToken` whose
  `cancel()` returns the call at once: `definition`, `references`, `hover`, `completion`,
  `resolve_completion`, `signature_help`, `document_symbols`.
- `diagnostics_settled(path, timeout)` waits until every server has reported on the
  current text. `with_file(path, &context, |manager| ...)` is for one-off questions about
  a file nobody has open: it opens from disk (or joins the editor's copy), asks, closes.
- `subscribe()` returns an `async_channel::Receiver<LspEvent>`:
  `DiagnosticsChanged { path }` (then read `diagnostics(path)`) and
  `ServerChanged(ServerStatus)`. States: `Starting`, `Ready`, `Indexing { title }` (from
  `$/progress`), `Failed { message, log_tail }`, `Stopped`, `ToolNotFound { looked_for,
  hint }`. A stopped or missing server nothing uses is reported once and then forgotten.

**Results** carry paths and `Span`s of `Point { line, column }`, the column in bytes.
Lines follow the protocol (`\n`, `\r\n` and lone `\r` all end one), so convert with
`LineIndex::new(text).offset_of(text, point)` rather than by counting lines yourself.
Positions are exact for UTF-16 servers (and UTF-8, which is negotiated when offered);
for files that are not open the text is read from disk to count. Diagnostics from all
servers of a document are merged in document order, each with `server` (`basedpyright`,
`ruff`) and the server's own `source` and `code`. Completion items come with snippet
syntax already reduced to plain text, since snippets are declined in the handshake.

**For the interface.** `document_diagnostics(path)` gives an open document's diagnostics
per server with the document version each report is about, and a report on a newer text
is announced (`DiagnosticsChanged`) even when it says the same as the last, so the editor
never lays a report over a text it was not made for. `set_project_settings(project,
ProjectSettings { disabled, disabled_servers, tool_paths })` and `set_limits(Some((idle,
max_servers)))` change a running manager: a disabled project's documents are not opened at
all (by the editor or by an agent's tool), a server switched off is left out, and a
project's executable wins over `ManagerOptions::tool_paths`. Both apply to documents
opened and servers started afterwards. A file inside the checkout's own virtual
environment counts as a library: it joins the server that pointed at it, or the
checkout's most recent one, like a file outside the checkout. The scripted server
(`lsp::fake`) is public behind the `test-support` feature for the interface's tests.

**Not there:** incremental sync (the whole text is sent per change), `didSave`, file
watching, workspace symbols, rename, code actions, formatting, semantic tokens, and
`workspace/applyEdit` (refused). Changing the settings or the toolchain needs the
documents reopened and the servers stopped, which the interface does.

## 5. Debug

`code::dap` is headless. The pane and the MCP tools call `Manager`, `Session` and
`BreakpointStore`; nothing else needs to be touched.

**Adapters.** `Adapter` (trait) + a static registry (`adapters()`, `adapter(id)`). Only
`Debugpy` exists: it runs `<Toolchain::python()> -m debugpy.adapter` over stdio after asking
that interpreter whether `debugpy` is importable; if not, `DapError::ToolMissing { tool:
"debugpy", hint: "… -m pip install debugpy" }`. An adapter gives: how to start it
(`AdapterStart`, stdio or a TCP port), the launch request for a `Spawn`, the attach-by-pid
request, its names for the exception filters, and where a child session connects. Go, JS
and LLDB are one more `impl Adapter` each.

**Run → debug.** `derive(&Spawn, &LaunchOptions) -> Option<DebugRequest>`:
`python file.py …` → `program`; `python -m mod …` / `-mmod` → `module`; `python -c code` →
`code`; `pytest …`, `py.test …`, `python -m pytest …` → `module: pytest`. Interpreter
options (`-u`, `-X dev`, `-W error`) go to `pythonArgs`; an interpreter named by path goes
to `python`, a bare `python` means the adapter's own. `cwd` and `env` are the spawn's.
`None` for anything else (`uv run …`, `node`, `python` with no program, `-m debugpy`).
`LaunchOptions { console, just_my_code: true, stop_on_entry: false, subprocesses: true }`.
`attach_to_process(pid, cwd, &options)` builds the attach-by-pid request; it is untested
against a real process (debugpy injects with lldb, which macOS may refuse).

**Console.** `Console::IntegratedTerminal` (default): debugpy sends `runInTerminal`; the
session emits `SessionEvent::RunInTerminal(TerminalRequest)` and keeps it in
`pending_terminal()`. The pane starts `request.spawn` (command, args, cwd, env to add to the
checkout's) in a terminal tab and calls `request.started(Some(pid))` or `.failed(reason)`;
the session stays `Starting` until then. `Console::InternalConsole`: debugpy starts the
debuggee and its output arrives as `SessionEvent::Output` / `session.output()`; for agents
and tests.

**Session.** States: `Starting`, `Running`, `Stopped(Stop { reason, thread_id, frame_id,
path, line, exception })`, `Terminated { exit_code }`. A start that fails ends in
`Terminated` with `failure()` set. Three kinds of call:
- *Cache, immediate, any thread:* `state()`, `cached_threads()`, `cached_stack(thread)`,
  `cached_scopes(frame)`, `cached_variables(reference, page)`, `output()`, `children()`,
  `driver()`, `supports(capability)`. On every stop the threads and the stopped thread's
  stack are fetched before the stop is announced. Frames, scopes and variables are dropped
  when the debuggee runs again.
- *Control, returns at once, runs on the session's worker thread:* `resume`, `pause`,
  `step(StepKind, thread)`, `terminate`, `disconnect`, `refresh(Refresh::…)`. A failure
  arrives as `SessionEvent::Failed { action, message }`.
- *Blocking, for background threads and MCP tools (10 s per request):* `threads()`,
  `stack(thread)`, `scopes(frame)`, `variables(reference, Option<Page>)`,
  `evaluate(expr, frame, Repl | Hover | Watch)`, `set_variable(reference, name, value)`,
  `stack_with_locals(frames)`, `wait_for_stop(timeout)`, `wait_for_exit(timeout)`,
  `continue_and_wait(timeout)`, `step_and_wait(kind, timeout)`. A wait that runs out returns
  `DapError::StillRunning`; the session is unaffected.

`subscribe()` gives each caller its own `async_channel::Receiver<SessionEvent>`:
`StateChanged`, `Stopped`, `Terminated`, `Threads`, `Stack`, `Scopes`, `Variables`, `Output`,
`RunInTerminal`, `ChildStarted`, `DriverChanged`, `Failed`. Events before the subscription
are not replayed: subscribe, then read `state()` and `pending_terminal()` once.
Capabilities are followed: hover evaluation, `setVariable`, conditions, hit conditions and
log points are only used when announced; debugpy cannot page variables, so a `Page` is cut
on our side.

**Child sessions.** `initialize` announces `supportsStartDebuggingRequest`, so debugpy 1.8
sends a `startDebugging` request per subprocess (the older `debugpyAttach` event is handled
the same way). The parent connects a second client to the adapter's TCP port, attaches with
the given configuration, and lists the result under `children()`; it shares the breakpoint
store, origin and driver, and ends with its parent. `Manager::get(id)` finds children too.

**Breakpoints.** `BreakpointStore` (cloneable handle, all calls cheap): `toggle`, `set`,
`remove`, `clear`, `in_file`, `files`, `exceptions` / `set_exceptions(ExceptionFilters {
raised, uncaught })`, `lines_changed(path, start, removed, inserted)` for the editor,
`to_json(under)` / `load_json`, `subscribe()` → `BreakpointChange::{File, Status,
Exceptions}`. Each `Breakpoint` has `line`, `enabled`, `condition`, `hit_condition`,
`log_message` and a runtime `status`: `Idle`, `Verified { line }` (the line the adapter
placed it on; draw it there) or `Rejected { message }`. Sessions send every file at start and
a file again on each change, and write the answers back; statuses return to `Idle` when a
top-level session ends.

**Manager.** `Manager::new(store)`; `start(&Spawn, &Toolchain, &LaunchOptions, Actor)`,
`start_request`, `restart(id)` (ends the session and starts its request again in the same
slot, new id; debugpy has no in-place restart), `sessions()`, `get(id)`, `remove(id)`,
`stop_all()`, and `stop_all` on drop. `start` and `restart` block for the adapter's start.
`subscribe()` gives an `async_channel::Receiver<ManagerEvent>` of `Added(id)` / `Removed(id)`
for top-level sessions, whoever starts or ends them (a restart is `Removed` then `Added`;
nothing is replayed, so read `sessions()` once after subscribing).
Ending a session sends `disconnect`, kills the adapter's process group and, for a debuggee
the adapter started itself, the debuggee's group. `Actor::{User, Agent(id)}`:
`session.origin()` is who started it, `driver()` who steers it, `take_over(actor)` changes
the driver (children included). The core does not refuse calls from a non-driver; the MCP
layer checks `driver()` before acting.

**Known behaviour.** With the `uncaught` filter on (the default), debugpy also stops on
`sys.exit(n)` for `n != 0`, as `SystemExit: n`. Windows: the debuggee is not force-killed.

## 6. Interface

- **File header:** location crumbs (`file › class › function`), back/forward, a run button
  with a configuration menu, a debug button, an error count.
- **Gutter**, left to right: breakpoint, run marker, line number, fold arrow, git bar.
- **Dock:** one tab per run (status dot, rerun, stop); one tab per debug session with stack,
  variables and console columns and the step controls in its header; the debuggee's output
  is an ordinary terminal tab beside it.
- **Right panel:** an Outline tab; find-usages results in the project search's result form.
- **Footer:** a chip for running work and one for language servers (state, memory, stop).
- **Quick open:** `Run: …` commands and symbol search.
- **Agent-driven debugging:** the session's rail row says where it stopped; the debug tab
  says the agent is driving and has a Take over button.

### Shell extension points

The features never edit `shell.rs`. They call `crate::code_shell` (`src/code_shell/`),
whose shell side is `shell/code_shell.rs`; `shell.rs` and its siblings hold one-line hooks
(the frame, the dock header, the footer, the project menu, the rail row). Each feature
installs itself with one line in `code_shell::register`, called once from `main.rs`.

| Module | What a feature gets |
| --- | --- |
| `dock` | `run(RunRequest, window, cx) -> Entity<Run>`: a dock tab whose child is the given process (`code::spawn::Spawn`), with the checkout's terminal environment and venv. `Run` has `state()`, `pid()`, `tail()`, `exited()`, `stop()`, `rerun()` and emits `RunEvent::{Started, Output, Exited, Closed}`. `add_tab(DockTab, …) -> DockTabHandle`: a tab showing a feature's view, with header controls, a close callback and its `DockPlacement`; `reveal`, `close`, `set_title`, `set_busy`, `find_tab`. |
| `panel_tabs` | `show(PanelTab, cx)`, `remove(id, cx)`, `reveal(id, window, cx)`: a right panel tab beside Explorer and History. |
| `chips` | `register(FooterChip, cx)`: a footer chip drawn while its `state` returns something, with a list on click. `code_shell::refresh(cx)` redraws it. |
| `processes` | `register(pid, label, kind) -> Registration`: a child process listed in the running work dialog with its memory and a Stop. |
| `commands` | `register_commands(id, source, cx)` for the `>` palette; `register_mode(Mode, cx)` for a prefix mode (`@`) with asynchronous rows. |
| `settings` | `register(SettingsPage, cx)`: a row in the project menu opening the feature's form in the project dialog; `PageHost::save` stores a JSON value per project and page, `settings::value` reads it. |
| `context` | `focused_checkout`, `focused_file`, `checkout_for_path`, `checkout_for_pane`, `terminal_setup`, `toolchain`, `open_file`, `open_url`; `events(cx)` emits `CodeEvent::{ProjectOpened, ProjectClosed, CheckoutAdded, CheckoutRemoved, FocusedFileChanged}`; `on_quit(cx, f)`. |
| `rail` | `set_status(session, text, cx)`, `clear_status(session, cx)`: a status on the session's rail row in place of its age. `session_label(session, cx)`: the session's title as the rail shows it. |

How it is built:

- **A run or view tab is a `Terminal` entity** in the dock's own list (`terminal/run.rs`,
  `terminal/hosted.rs`), so selection, reorder, close, close-others and moving the dock
  work unchanged. A run's PTY is wrapped to copy its output out; the copy is stripped of
  escape sequences and capped at 64 KiB. Stop sends SIGTERM to the process group and
  SIGKILL after three seconds. Neither kind of tab can be pulled out into a pane, and
  neither is saved (`TerminalDock::transient`): a restart reopens the shells only.
- **Registries are GPUI globals** read while the shell draws; a contribution changes what
  is drawn at the next frame. Processes are a process-wide list, usable from any thread.
- **The context is a snapshot** refreshed once a frame, so lookups are safe from anywhere;
  events are the difference between two snapshots.
- **Calling rules.** A function taking a `Window` borrows the shell: call it from a
  feature's own entities, tasks and click handlers, not from `FooterChip::state` or
  `items`. Callbacks the shell makes with a `Window` (a picked command or chip row, a
  settings page being built) run after the shell's update, and may call anything.
  `code_shell::in_window(cx, f)` supplies the window to a task that has none.
- **Tests.** A run starts a real process even under test, so a test that makes one calls
  `cx.executor().allow_parking()`. `shell/code_shell_tests.rs` drives every extension
  point with a fake feature and is the place to copy a usage from.

Not covered: feature tabs do not appear in the Original layout's sidebar tool tabs, and
the rail status shows in the project rail's tree only.

### Editor extension points

The three parts draw in the file editor through `crates/monocode-gpui/src/code_editor/`
and never edit `input/code.rs` or `file_editor.rs`. Everything is UI-thread GPUI code;
a provider returns at once and hands back a `Task` for anything that waits. An editor
nothing is installed on allocates none of this and looks as before (review, consoles,
notes).

**The hub** (`code_editor::hub(cx) -> Entity<Hub>`, one per application) is where editors
and features meet.

- *Document events:* `cx.subscribe(&hub, |.., event: &DocumentEvent, ..|)` delivers
  `Opened`, `Changed`, `Saved`, `Closed` with `doc: Rc<DocumentInfo>` (absolute `path`,
  checkout `root`, `language`), the full `text` and a `version` that starts at 1 and
  grows by one per `Changed`. Changes are sent 200 ms after typing pauses, and always
  before a save and before any provider is asked. `hub.open_documents()` lists what is
  open for a subscriber that starts late.
- *Per-file state*, keyed by path and mirrored into every editor on that file, now or
  when it opens: `set_diagnostics(path, source, version, items)`,
  `set_gutter_marks(path, slot, marks)`, `set_line_decorations(path, source, lines)`,
  `set_inline_notes(path, source, notes)`, `reveal(path, row)`. Rows are zero-based,
  diagnostic ranges are bytes of the text at `version`. The editor carries all of it
  through edits (diagnostics with `Edit`, rows with `RowShift`) until it is replaced.
  A diagnostics list for a version the document has left is dropped.
- *History:* `jump(from, to)`, `back(current)`, `forward(current)` over `Location`
  (path, one-based line and column), 50 entries.
- `set_references_handler` takes "Find usages" results instead of the editor's list.

**Providers** are traits a feature implements and installs through
`code_editor::register(cx, |doc, providers, cx| ...)`, which runs for every file editor
(and again for open ones), or per editor with `Input::set_editor_providers`:

| Slot in `Providers` | Trait | Asked |
|---|---|---|
| `completion` | `CompletionProvider` | 100 ms after a word character or one of its trigger characters, or Ctrl+Space; answers the caret has left are dropped; items are narrowed and ordered by the editor |
| `hover` | `HoverProvider` | mouse at rest 350 ms on a word, or F1 at the caret |
| `signature` | `SignatureProvider` | on `(` and `,`, then at every caret move until it answers `None`; Cmd+Shift+Space |
| `links` | `LinkProvider` | Cmd-hover (underline), Cmd-click, F12, context menu |
| `references` | `ReferencesProvider` | Shift+F12, context menu |
| `breakpoints`, `run_markers` | `GutterProvider` | a press in its gutter column (`menu` first: a `GutterMenu` the editor opens at the press, else `click`); an edit moved its marks |
| `header` | `HeaderProvider` | every draw of the header: buttons, optional crumbs |

The built-in completers (QuickJS, the SQL console's) still answer when no provider is
installed or it returns nothing.

**Gutter**, left to right: breakpoint column, run column (14 px each, only while owned
or marked), line number, fold arrow. A left press on an empty breakpoint row arrives as
`GutterClick { mark: None, button: Left }`: toggle.

**Header:** back/forward arrows once there is history (Ctrl+- and Ctrl+Shift+-), the
file name followed by the crumbs of the caret's enclosing symbols, the problem count
(opens the file's problems; picking one jumps), contributed buttons, then Wrap.

**Symbols** (`code_editor::symbols`, Tree-sitter only): `outline(path, text)`,
`chain_at(&outline, offset)`, `enclosing(path, text, offset)` for Python, JavaScript,
TypeScript, Rust, Go, Java, C/C++ and Ruby, and `python_runnables(text)` with pytest and
unittest rules plus the `__main__` guard. `FileEditor::outline()` is the open file's.

### Run in the interface

`crates/monocode-gpui/src/run_ui/`, attached only through the extension points above.

**Starting** (`start.rs`). `run_ui::start(config, context, checkout, cx)`, `debug(…)`,
`start_for_agent(session, …)` and `rerun_last(cx)` all go through `begin(Request, cx)`,
which returns at once with an `Outcome` (a receiver of `Result<RunId, String>`): the focused
file is saved if it has edits, the configuration is resolved on a background thread with
the checkout's toolchain, and then on the UI thread the process gets a dock tab
(`code_shell::dock::run`) and an entry in `services().runs()`. The tab's events feed the
entry (`set_pid`, `append_output`, `finish`; a stop from the tab's header is recorded as
`Stopped`), the process is listed with `code_shell::processes`, and
`RunEvent::StopRequested` from the registry stops the tab, so an agent's `run_stop` and the
footer work on tab runs. A run that cannot be resolved or started shows the core's sentence
where the shell shows its own errors ("“Serve” cannot run. No file is focused.").

- *Run again while running:* starting the same resolved process (same command, arguments,
  folder and environment, same checkout, same starter) restarts it in its tab, as the tab's
  own rerun button does: the old process is killed and recorded `Stopped`, the new one is a
  new registry entry. The keyboard stays where it was. A different process of the same
  configuration (another `$FILE`) gets its own tab; that configuration's finished tabs are
  closed first, so they do not pile up.
- *Last run:* a user's run is stored with `ProjectRuns::mark_used` and saved; an agent's is
  not. `rerun_last` repeats the last resolved process of this app session.
- *Debug* resolves the same way and calls `code_shell::launch::debug`; it is not a run.

**Header** (`header.rs`). A Run button on files that have built-in tasks or whose project
has configurations, and a Debug button beside it while a debugger is installed. A click
starts the project's last run if it is offered here, else the first built-in task for the
file, else the first configuration; a built-in task counts as offered only in the file it
was made for, so the button follows the file while a user's configuration sticks. The
dropdown lists the offered configurations, the file's tasks (the default ticked) and
"Edit configurations…". Clicks get the caret and selection from the editor; the dropdown is
listed from the document and the caret of the last draw, so a configuration that needs
`$SELECTED_TEXT` shows in quick open but not there.

| Key | Does | Where |
|---|---|---|
| Alt+Shift+R | Run the focused file's default | file editor |
| Alt+Shift+D | Debug it | file editor |
| Ctrl+Alt+R | Run the last run again | anywhere |

**Gutter** (`gutter.rs`). Python documents are searched off the UI thread on every open
and change (`symbols::python_runnables`), mapped to the core's `Runnable`s (tests only in
`test_*.py` / `*_test.py`, the `__main__` guard anywhere, nothing the project's test runner
cannot address) and pushed as `GutterSlot::Run` marks with tooltips. A click starts the
first of `runnable_tasks`; a right click lists them all, each again as "Debug …" with a
debugger installed. `$ROW` and `$SYMBOL` are the marker's.

**Configurations** (`settings_page.rs`). Project menu › "Run configurations…": the list
with add, duplicate, remove and move up/down, a form (name, command, arguments, working
folder, environment), the command line it comes to for the focused file or the sentence
saying what is missing, the variables, and pytest/unittest. List changes save at once,
typing with Save. The store is `Workspace::project_runs` through
`code_shell::settings::{runs, save_runs}`; a configuration with no command yet stays on
the page unsaved, and deleting a project drops its entry.

**Footer chip** (`chip.rs`, `output.rs`). "N running" while runs of the focused project
are running, hidden otherwise. Its list has those and the last eight that ended: label
(and worktree), state or exit code, time taken, and "you" or the agent session's title;
each row shows its tab, stops it, runs it again (as the user, in a tab) or opens the local
address it announced in the browser pane, which never happens by itself. A run with no tab
(an agent's `run_start`) opens a read-only dock tab that follows the registry's output.

**Quick open** (`commands.rs`). `Run: <name>` for every offered configuration and task,
`Run: Rerun last`, `Run: Stop <label>` per running run, `Run: Edit configurations`.

Not there: dragging to reorder configurations, a "running" state on the header button, an
"Open in browser" control on the run tab's own header (it is on the chip's row), and
markers for Go, Rust and JavaScript (their file tasks are in the header menu).

### Code insight

`code_insight/` is the language servers' face, built only on the extension points above.
`init` uses `services().lsp()`; `init_with(manager)` is the same over any manager (tests).

- **Documents** (`sync.rs`). The hub's `Opened` / `Changed` / `Closed` become the
  manager's `open` / `change` / `close`, one `open` per hub document however many panes
  show it. Only files the registry has a server for, up to the editor's lint limit
  (512 KiB) and with no line over 20,000 bytes. The `DocumentContext` comes from
  `services().context(path).document()`, built on a background thread and kept per
  checkout until the project's terminal setup changes. A file outside every checkout is
  opened under the checkout its pane came from, so a library reached by "Go to
  Definition" is served by the server that pointed at it.
- **Diagnostics.** On `DiagnosticsChanged` each server's list is converted with
  `LineIndex` against the text of the version it is about and stored in the hub under the
  server's id (`basedpyright`, `ruff`); a list about an older version is skipped. The
  message carries the rule code: `` `os` imported but unused (F401) ``.
- **Providers** (`providers.rs`), installed for every file with a server: completion
  (trigger characters from the manager; kind, detail, replace range, filter and sort
  keys, extra edits such as an import; documentation through `resolve_completion` when
  an item is selected), hover, signature help, definition (`navigable` asks for the
  definition) and usages. Each request is one blocking call on the background executor
  with a `CancelToken` cancelled when the editor drops the task. While a file's server is
  still starting, the requests the editor makes unprompted answer nothing at once
  instead of waiting. Paths in answers are respelled the editors' way when the server
  resolved a link in the checkout's path (`/private/var` for `/var`).
- **Completion while typing.** `code_editor::carry_completions`: the list an edit closes
  is kept and, when the edit was a word character typed or deleted at the caret, shown
  narrowed with its ranges moved until the provider's new answer replaces it.
- **Usages tab** (`usages.rs`, panel tab `code-usages`). The hub's references handler
  takes results of more than 6 places: grouped by file, line number and line text with
  the symbol marked, the symbol and counts in its header, Up / Down / Enter, a close
  button. A row opens through `code_editor::open_location` and records a history jump.
  Smaller results stay in the editor's own list.
- **Outline tab** (`outline.rs`, `code-outline`, always there). The focused file's
  `FileEditor::outline()` (Tree-sitter): follows `FocusedFileChanged`, marks the caret's
  innermost symbol, filters by substring keeping enclosing symbols, a row or Enter
  jumps. The header crumbs are the file pane's own; nothing is added to them.
- **Symbol search** (`symbol_search.rs`). Quick open's `@`: the focused file's symbols,
  then, once something is typed, those of the checkout's other files of the same
  language: a breadth-first walk skipping dot folders and vendored ones, at most 1,500
  files of 512 KiB and 700 ms. Prefix matches first, then inside the name, then letters
  in order.
- **Chip** (`chip.rs`, `code-servers`). The servers of the checkout in front and of the
  file in front: hidden with none, quiet when ready (`2 servers`), busy while starting or
  indexing, a warning for a missing tool or a failed server. Its list has a row per
  server (name, root, state, open files, memory summed over the server's process group
  every 10 s) with Restart and Stop; a missing tool adds what was looked for and the
  install hint, a failed one its message, both copyable, and "Set its path…" opens the
  settings. "Look again" on either rebuilds the checkout's context from disk first.
  Running servers are registered with `code_shell::processes`.
- **Settings** (`settings.rs`, page `code-insight`). Per project: on or off, each server
  on or off, an executable per server. Saving applies it: the project's documents are
  closed, its servers stopped, and what may be open is opened again. The idle minutes
  and the server cap are the app's; saving them writes them to every open project.
- **Lifecycle.** `CheckoutRemoved` and `ProjectClosed` close the checkout's documents
  with the manager and stop its servers; the editors stay.

Not there: no `@` search without a focused file; hover, completion and navigation in a
project with code insight off fall back to the editor's own; unused and deprecated code
is not drawn differently; the usages list is not updated when files change.

### Debug in the interface

`crates/monocode-gpui/src/debug_ui/` draws the debugger of §5 and owns no debugger state
of its own: it reads `services().dap()` and `services().breakpoints()`, the instances the
agents' tools use, so either side sees what the other does.

| File | Holds |
| --- | --- |
| `breakpoints.rs` | the gutter column of Python files, the breakpoint menu and form, persistence |
| `sessions.rs` | `Debugger`: every top-level session of the manager, one model and dock tab each |
| `model.rs` | `Model`: one session and its subprocesses as the tab shows them, and what a stop does outside the tab |
| `tab.rs`, `columns.rs` | the dock tab: header controls, the three columns |
| `launch.rs` | the launcher behind `code_shell::launch::debug`, the notice when a start fails |
| `hover.rs`, `values.rs` | the hover wrapper, inline values, the expression under the mouse |
| `keys.rs` | key commands and quick open commands |

**Breakpoints.** A left press in the column toggles a line breakpoint; a right press
opens a menu with the condition, hit count and log message as a form (stored on Enter and
when the menu closes), Enable / Disable and Remove; on an empty row it sets one first. Marks
are drawn from the store on every change of it, at the adapter's verified line when it
moved one: enabled, disabled, conditional (any of the three texts), unverified (rejected).
When an edit moves marks, the editor's rows are written back to the store as one
replacement of the file's list, and the store's echo of that write is not drawn again, so
typing never fights the marks. Each project's breakpoints (its folder and its worktrees)
and the exception filters are saved 400 ms after a change as the project's code setting
`debug-breakpoints` (`BreakpointStore::to_json`) and loaded on `ProjectOpened`.

**Sessions.** `Debugger` follows `dap::Manager::subscribe()`. Each top-level session gets
a `Model` and a dock tab in the checkout its `cwd` is in; a session an agent started gets
its tab without the dock being raised. The same request started again (a restart, a second
run of the same program) takes the tab its ended run left. Closing the tab removes the
session from the manager and stops the debuggee's terminal.

**The tab.** Header: continue or pause, step over, into, out, restart, stop, enabled by
the selected session's state; while an agent is the driver they are off except Stop, and
"driven by <session title>" with **Take over** (`Session::take_over(Actor::User)`) is
shown. Body, three resizable columns, stacked when `DockPlacement::narrow()`: the stack
(processes when there are subprocesses, threads when there are several, frames; library
frames and files outside the checkout dimmed) over the project's breakpoints and the two
exception filters; the variables of the selected frame as a tree that stays open across
steps (kept by name path), cheap scopes open, a sequence with more than 100 indexed
children listed 100 at a time behind "Show more", copy and (with `supportsSetVariable`)
edit on hover; the console with the adapter's output, evaluation in the selected frame
(`Repl`) and its history on the arrow keys. The lists are `uniform_list`s over rows the
model rebuilds from the session's caches on each event; nothing waits on the UI thread.

**A stop.** The stopped line gets `LineStyle::Execution` with the arrow, a selected frame
that is another line `LineStyle::Frame`; the file opens at the stop with a history jump
and the tab is revealed. For a session an agent drives the keyboard is given back to where
it was, an open file is only scrolled, and nothing is revealed unless the session's project
is in front. An exception's text is shown across the top of the tab. The selected frame's
locals are written after the lines of its function that name them (from the `def` to the
stop; text only, values over 48 characters cut). `SystemExit` under the uncaught filter is
continued automatically unless "raised" is on or an agent drives. The agent's rail row
says "debugging" or "paused at file:line" until the session ends or is taken over.

**The debuggee's terminal.** `SessionEvent::RunInTerminal` starts `request.spawn` with
`code_shell::dock::run` and answers `started(pid)` or `failed(reason)`.

**Hover.** Registered after code insight, the provider wraps the one in `Providers.hover`:
in the file of the selected frame of a paused session the name under the mouse, with its
owners (`self.items`), is evaluated (`EvaluateContext::Hover`) and shown above the wrapped
provider's text; anywhere else the wrapped provider answers alone.

**Keys.** F5 continue / pause, F10 step over, F11 step into, Shift+F11 step out, Shift+F5
stop, Cmd/Ctrl+Shift+F5 restart, bound everywhere and passed on when no session takes
them; F9 toggles a breakpoint at the caret in a file editor. Quick open: `Debug: <file>`
for the focused Python file, the controls that apply, Take Over, Remove All Breakpoints.

**Not there.** A selected expression is not evaluated on hover (the hover seam carries
no selection); watch expressions; data and function breakpoints; attach to a process from
the interface; F5 does not start a session; the step buttons reuse existing icons.

## 7. Agents (MCP)

**Shared services.** `code::services` is the one home of the instances the panes and the
tools share. `services() -> &'static Services` (made on first use, any thread) gives
`lsp() -> &lsp::Manager`, `dap() -> &dap::Manager`, `breakpoints() -> &BreakpointStore` and
`runs() -> &run::Registry` (the same registry as `run::registry()`). `shutdown()` stops debug
sessions, language servers and headless runs; the app calls it at quit.

`Services::context(path) -> CodeContext { checkout, project, setup, runs }` says where an
absolute file or folder belongs and what its project has: the `TerminalSetup` and the
`ProjectRuns`. `CodeContext::toolchain()`, `.document()` (an `lsp::DocumentContext`) and
`.run_context()` build what the cores take. The answer comes from a resolver the app
registers with `set_resolver`; it is called on any thread, the interface's included, so it
must not wait. Without one, or for a path it does not know, `locate(path)` decides by the
disk: the checkout is the nearest folder with `.git`, the project that checkout or, for a
linked git worktree, the repository it was added to; the setup and runs are the defaults.

`monocode-gpui/src/code_agent.rs` is the app's resolver. `install(&shell, cx)` (one line in
`main.rs`) keeps a copy of the workspace's project list, terminal setups and run
configurations behind a lock, refreshed whenever the shell notifies and something in them
changed, and answers from the copy: the innermost project folder holding the path, else
the project whose linked worktree the path is in. A project that is a folder inside a
larger repository is its own checkout. It also applies the setting below and registers
`shutdown` for quit.

**The server.** `code::bridge`, built like `browser_bridge.rs`: a listener on `127.0.0.1`,
a random token per agent session, and the stdio subcommand `code-mcp` of the Monocode
binary, which each provider is handed when a session starts (Claude `--mcp-config`, Codex
`mcp_servers.monocode_code.*`, ACP `mcpServers`) and which forwards every `tools/call` as
one JSON line `{session, token, tool, arguments}`. Server name `monocode_code`. Unlike the
browser, a call needs no interface thread: it is answered on its connection's thread and
may wait there for a program (at most 300 s; the stdio side waits a minute longer).

**Capability.** `bridge::environment(session, cwd, project, executable)` is called by
`runtime.rs` for a local session that is not text-only. The token maps to the session id,
the folder the agent runs in and its project. Every path a tool is given is made absolute
(relative ones from the session's folder), has `..` and symbolic links resolved, and must
lie inside the session's folder, its project or its checkout; anything else is refused.
Runs and debug sessions of other projects do not exist as far as the tools can tell. No
answer is longer than 30,000 bytes. Settings › Providers › "Code tools for agents"
(`Preferences::code_agents`, on by default) turns the capability off for sessions started
afterwards.

**Tools.** Answers are plain text; paths are shown from the checkout; lines and columns
count from 1.

| Tool | Does | Asks first (Claude) |
|---|---|---|
| `code_servers_start` | Starts the language servers for a file's project and waits until they are ready, or reports the missing tool with its install hint; they stay up (a lease of 15 minutes from the agent's last use, renewed by each insight call) | yes |
| `code_diagnostics` | Errors and warnings for up to 20 files once the running servers have settled (20 s default); columns are characters from 1 | no |
| `code_definition` | Definition of the symbol at `file` + `line` + `column` (in characters), or + `symbol` (a name on that line): path, line, column, the line's text | no |
| `code_references` | Every use, the same way; at most 120 listed | no |
| `code_hover` | Type, signature and documentation | no |
| `code_symbols` | A file's outline with line ranges | no |
| `run_list` | The project's configurations and the built-in tasks for a file or test, each as the command it becomes in this checkout | no |
| `run_start` | A configuration or task by `name`, or a `command` line (+ `cwd`, `env`); headless, `StartedBy::Agent`; `wait_s` waits for the exit code and output | yes |
| `run_output` | Output and state, incrementally with a `cursor` | no |
| `run_stop` | Stops a run this session started | no |
| `run_status` | Running and recently ended runs of the checkout, the user's included, with local addresses | no |
| `debug_start` | `run_start`'s targets under debugpy (`internalConsole`), `breakpoints` set first; returns at the first stop or the end | yes |
| `debug_breakpoints` | `list` / `set` / `clear` plain line breakpoints by file and line; the editor's store. A condition, hit condition or log message is refused here | no |
| `debug_breakpoint_condition` | A breakpoint with a `condition`, `hit_condition` or `log_message`: the debuggee evaluates them, so it is like `debug_evaluate` | yes |
| `debug_continue`, `debug_step` | Continue (or keep waiting), step `over` / `in` / `out`; return the next stop or the end | no |
| `debug_stack` | Frames with ids and the locals of the top 1 to 5 | no |
| `debug_variables` | The children of one `ref`, paged | no |
| `debug_evaluate` | An expression in a frame (it can change the program) | yes |
| `debug_stop`, `debug_sessions` | End a session; list the project's sessions | no |

A stop is reported as the reason, file and line, three source lines either side, up to 12
frames, the top frame's locals (40 at most, each value cut at 240 bytes, `ref=N` where it
can be expanded) and what the program printed since the last report. An end is the exit
code and that output. A wait that runs out says the program is still running and changes
nothing.

**Who steers.** Sessions an agent starts have `Actor::Agent(session id)` as origin and
driver. `debug_continue`, `debug_step`, `debug_evaluate` and `debug_stop` check
`Session::driver()` and refuse, saying so, once the user has taken the session over;
`debug_stack`, `debug_variables` and `debug_sessions` keep working. A run is stopped only
by whoever started it.

**Language servers are started only on request.** A language server is a program from the
project's own environment, so a pre-allowed tool must not start one (an agent allowed to edit
but not to run commands could plant `.venv/bin/basedpyright-langserver`). The `code_*` insight
tools join only servers that already run (the editor's, or those `code_servers_start`
started: `lsp::Manager::with_running_file`) and otherwise answer that no server is running
and which tool to call. The question is put in the text the manager holds for the file,
which is the editor's when it has the file open with unsaved changes. Servers started for an
agent carry a lease of 15 minutes (`Manager::start_servers`, renewed by that agent's
later calls) that the idle sweep respects.

**Trust.** Program output, variable values, evaluation results and hover text are the
project's own text and can be written to steer an agent. They are returned under a line
saying they are data, and the server's instructions and each such tool's description say
the same. Claude is given the reading and steering tools ahead of its permission mode
(`bridge::claude_allowed_tools()`); `run_start`, `debug_start`, `debug_evaluate` and `debug_breakpoint_condition` run the
project's code and go through the session's permission mode like a shell command.

**Not there.** Completion and signature help (an editor's needs), attach to a process,
exception filters, and remembering what an agent ran as the project's last
run. A file the editor shows under another spelling of its path than the resolved one is
opened a second time by a tool rather than joined.

## 8. Verification

**Run in the interface.** `cargo test … -p monocode-gpui run_ui -- --test-threads=1`: ten
GPUI tests in `run_ui/tests.rs` on a real shell and a temporary project whose `.venv` holds
a stand-in interpreter: a file task run with unsaved edits (saved first; tab, registry
entry, output, exit code, last run, reuse of the tab on rerun); an unresolvable
configuration's sentence; a stop through the registry and a restart while running; the
gutter's marks, a real click on one and the `Spawn` it starts, its menu with and without a
debugger; the header's buttons, menu inside and outside a package, default and key; the
settings page into `Workspace::project_runs` and back through a restore; the chip with an
agent's headless run, its output tab, stop, rerun and removal; the quick open commands; an
agent's run in a tab. With `MONOCODE_TEST_PYTHON`, one test runs real pytest from a marker
(a passing and a failing test, exit codes 0 and 1, pytest's output). Nothing was looked at
in the running app: the page's and the output tab's layout, the right-click menu on a
marker and opening a detected address in the browser pane are untested beyond compiling.

_To be filled in: what was run, by hand and by test._

### Code insight

Nothing here was looked at in the running app; all of it is GPUI tests (state, and
headless paints where noted).

- `code_insight/tests.rs`, a file pane over scripted servers (`lsp::fake`): diagnostics
  of two servers land under their own source on the right bytes of a text with
  non-ASCII characters, follow typing before any server hears of it, a late report on
  the old text is not shown, and closing the pane closes the document and clears them;
  completion narrowed and ordered, documentation resolved on selection, the list kept
  while one more letter is typed, the import applied with the item; hover and
  signature help; two usages stay in the editor's list, eight open the Usages tab (which
  paints), and a row is a jump Back returns from.
- `code_insight/shell_tests.rs`, a whole shell: go to definition into another file the
  server names by its resolved path opens it in a pane under the editors' spelling and
  is served by the same server, Back returns, usages reveal the tab in the right panel
  and a row opens its file; the Outline follows the pane in front, marks the caret's
  symbol, filters, jumps and paints, and `@` lists the file's and the checkout's
  symbols; the chip is hidden, quiet, busy, warns for a missing tool (hint copied) and
  for a failed start; the settings page sets a path, disables the project (no document
  open, no server, none started later), enables it again; removing the project closes
  its documents and stops its servers.
- With `MONOCODE_TEST_PYTHON`: the real basedpyright through the real manager marks a
  type error in a pane, its processes are listed as running work and measured, and
  go to definition opens the other file at the definition.
- Core: `code::lsp::manager_tests` cover the per-server versions, the project settings
  and limits, and the virtual environment's libraries.

**Debug in the interface.** By test only; the app was not launched. `debug_ui/tests.rs`
drives a real shell and real file editors against the core's scripted adapter
(`dap::fake`, behind the core's `test-support` feature, through
`Manager::start_scripted`): a gutter press toggling the store and the marks, an agent's
breakpoint appearing, edits above moving breakpoints, the right-press menu and its form,
the per-project save and reload; a session from its first stop to its end (tab, title,
arrow on the stopped row, file opened, stack with dimmed library frames, variables, a
paged list, inline values, console evaluation, set variable, hover while paused and
after, a step, a selected frame, F5 to the end, marks cleared); the `runInTerminal`
request answered with the pid of a real dock run and that run stopped by closing the tab;
an agent's session (tab not raised, rail status, controls off, Take over); the
`SystemExit` rule both ways; the notice for a program that is not Python.
`debug_ui/real_tests.rs`, with `MONOCODE_TEST_PYTHON`, starts a script through
`code_shell::launch::debug` under the real debugpy with its debuggee in a dock terminal:
breakpoint, locals, step, restart in the same tab, run to exit code 0. What tests cannot
show is how the tab looks: column proportions, colours, the icons chosen for the step
buttons and the drag of the dividers were never seen.
