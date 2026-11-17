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
| `breakpoints`, `run_markers` | `GutterProvider` | a press in its gutter column; an edit moved its marks |
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

## 7. Agents (MCP)

_To be filled in: tools, capability per session, as `browser_bridge.rs` does._

## 8. Verification

_To be filled in: what was run, by hand and by test._
