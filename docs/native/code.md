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
  base whose text ends in `TestCase`; at module level or nested in another test class.
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

**Not there:** incremental sync (the whole text is sent per change), `didSave`, file
watching, workspace symbols, rename, code actions, formatting, semantic tokens, and
`workspace/applyEdit` (refused). Changing `tool_paths` or the toolchain needs the
documents reopened.

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
| `code_diagnostics` | Errors and warnings for up to 20 files once the servers have settled (20 s default) | no |
| `code_definition` | Definition of the symbol at `file` + `line` + `column`, or + `symbol` (a name on that line): path, line, the line's text | no |
| `code_references` | Every use, the same way; at most 120 listed | no |
| `code_hover` | Type, signature and documentation | no |
| `code_symbols` | A file's outline with line ranges | no |
| `run_list` | The project's configurations and the built-in tasks for a file or test, each as the command it becomes in this checkout | no |
| `run_start` | A configuration or task by `name`, or a `command` line (+ `cwd`, `env`); headless, `StartedBy::Agent`; `wait_s` waits for the exit code and output | yes |
| `run_output` | Output and state, incrementally with a `cursor` | no |
| `run_stop` | Stops a run this session started | no |
| `run_status` | Running and recently ended runs of the checkout, the user's included, with local addresses | no |
| `debug_start` | `run_start`'s targets under debugpy (`internalConsole`), `breakpoints` set first; returns at the first stop or the end | yes |
| `debug_breakpoints` | `list` / `set` / `clear` by file and line, with a condition; the editor's store | no |
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

**Trust.** Program output, variable values, evaluation results and hover text are the
project's own text and can be written to steer an agent. They are returned under a line
saying they are data, and the server's instructions and each such tool's description say
the same. Claude is given the reading and steering tools ahead of its permission mode
(`bridge::claude_allowed_tools()`); `run_start`, `debug_start` and `debug_evaluate` run the
project's code and go through the session's permission mode like a shell command.

**Not there.** Completion and signature help (an editor's needs), attach to a process,
exception filters, log points, and remembering what an agent ran as the project's last
run. A file the editor shows under another spelling of its path than the resolved one is
opened a second time by a tool rather than joined.

## 8. Verification

_To be filled in: what was run, by hand and by test._
