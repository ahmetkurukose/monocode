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

_To be filled in: configuration format and storage, variables, built-in tasks, API._

## 4. Code insight

_To be filled in: servers per language, lifecycle and memory policy, API._

## 5. Debug

_To be filled in: adapters, session model, breakpoints, API._

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
| `rail` | `set_status(session, text, cx)`, `clear_status(session, cx)`: a status on the session's rail row in place of its age. |

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

## 7. Agents (MCP)

_To be filled in: tools, capability per session, as `browser_bridge.rs` does._

## 8. Verification

_To be filled in: what was run, by hand and by test._
