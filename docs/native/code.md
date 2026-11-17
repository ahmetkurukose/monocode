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

## 7. Agents (MCP)

_To be filled in: tools, capability per session, as `browser_bridge.rs` does._

## 8. Verification

_To be filled in: what was run, by hand and by test._
