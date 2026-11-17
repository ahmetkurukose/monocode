# Monocode Native preview

Monocode Native is the experimental Rust + GPUI app. The production app still
uses React/Tauri. Native builds have their own Cargo workspace and lockfile.

## Requirements

- Node.js 22+ and a current stable Rust toolchain.
- On macOS, Xcode with macOS components.
- Linux core checks need D-Bus headers and pkg-config (`libdbus-1-dev` and
  `pkg-config` on Debian/Ubuntu).
- At least one installed, signed-in agent CLI. See [provider setup](../providers.md).

The native commands do not require `npm install` or a frontend build.
macOS is the primary preview target. A Linux ARM64 source build has been
smoke-tested in a VM; Linux x64 packages and Windows GUI interaction still need
runtime testing. See [preview limitations](parity.md).

Fenced `mermaid` blocks render as native diagrams in Markdown previews and
conversations. See [Mermaid diagrams](mermaid.md) for examples and limitations.
For interactive visuals, use `/visualize` in an idle local desktop conversation
or ask a Mono directly. See [session visuals](visuals.md) and the
[preview release notes](preview-release-notes.md).

## Monos

A Mono's **Overview** tab brings together its current activity, a contribution
calendar, today's work, upcoming habits and new documents. **Sub-agents** lists
the sessions it launched, with their model, provider, checkout and state. Monos
are prompted to delegate longer tasks to background sessions, whose final
reports return to the Mono chat.

Contribution counts come from the Mono's sessions, including saved history:
commits and pull requests opened, merged or reviewed through supported agent
tools. Opened pull requests are checked for later merges through the GitHub CLI.
Records are stored in `monocode.db.gpui.sqlite3` alongside native preferences
when using the installed desktop database.

In the Mono's **Settings → General → Heartbeat** section, **Check in on its own**
controls inactivity checks. They are enabled by default with a 30-minute quiet
interval; choose 15 minutes, 30 minutes, 1 hour or 2 hours, or turn them off.
While the native app is running, an idle Mono with an existing conversation can
review sessions, notes and inbox changes in its assigned projects and suggest
up to two follow-ups. It can also perform recurring checks already described
in its soul's standing instructions. New work is proposed for the user to accept.

Heartbeats keep the Mono's selected permission mode and use its provider account,
with a smaller model when its catalog offers one. Quiet checks leave no report
or retained side session, and repeated quiet results extend the interval up to
four hours. Recent suggestions and the last check time are saved in the Mono's
`heartbeat.json` file to avoid repeating
the same follow-ups after a restart. Scheduled habits take priority over
heartbeats. Live scheduler and provider behavior still need broader testing;
see [verification](verification.md).

## Worktrees

A worktree is a separate working copy of a local Git project on its own branch,
so several sessions can change the same repository without stepping on each
other. A session uses a worktree only when you ask for one: **New worktree…**
in the worktree switcher, a project's rail menu, **Settings → Worktrees →
Create worktree**, or the composer's "New worktree".

- **Where they live.** New worktrees go under one root,
  `~/monocode/workspaces/<repository>/<name>`. Change it in **Settings →
  Worktrees → Location**; the `MONOCODE_WORKTREE_ROOT` environment variable,
  when set, replaces the default. The same page sets a branch prefix, each
  project's default base and whether other worktrees are shown.
- **Create dialog.** Pick a name (the folder and branch follow it), where to
  start from (the project's default base, a local or remote branch, or a
  commit), and optionally make that base the project's default. **Advanced**
  takes an explicit branch name and can check out an existing branch. The
  dialog closes at once; creation runs in the background with a pending row
  in the rail and the switcher, and a failure keeps the row with **Retry** and
  **Dismiss**. The new worktree becomes the project's current one.
- **`monocode.yaml`** at the repository root (or Orca's `orca.yaml`, same
  schema, when there is no `monocode.yaml`) configures new worktrees:
  `worktree.sharedDirectories` lists ignored folders (for example
  `node_modules`) linked from the main checkout instead of copied, and
  `scripts.setup` is a script run in a **Setup** terminal in the new worktree.
  A new or changed setup script is shown in the dialog and runs only once you
  choose **Run setup**, which trusts that version for the repository.
- **`.worktreeinclude`** at the repository root lists ignored files (one
  relative path per line, such as `.env`) copied into each new worktree.
- **Rail tree.** With **Project list: Tree** (Classic layout), an open project
  lists its worktrees after its own sessions: name, branch and session count,
  with the current one highlighted. Open a worktree to see the sessions
  working in it; a click makes it the project's current worktree. Its menu
  offers Open, New session here, Copy path, Reveal and Delete…. Worktrees
  made outside Monocode are listed only when **Show other worktrees** is on.
- **Delete.** The Delete dialog lists uncommitted changes, ignored local files
  that would be lost and unpushed commits; a dirty worktree needs **Force
  delete**. Its sessions are kept unless you also delete them. A managed
  worktree's branch is deleted with it only when Git considers it merged; an
  unmerged branch is kept and a card offers **Delete branch**.
- **Older worktrees.** Worktrees created by earlier versions next to the
  project (`<project>-worktrees/`) stay where they are, keep working and are
  listed as legacy; their branches are never deleted with them.
- **Remote (SSH) projects** create worktrees on their host from a reduced
  dialog (branch, start from, existing branch), without shared folders,
  included files or setup.

Design notes: [worktrees.md](worktrees.md).

## Develop

From the repository root:

```sh
npm run native:dev
```

Development uses a separate database. To open a particular project:

```sh
npm run native:dev -- --cwd /path/to/project
```

Type `r` then Enter in the terminal to rebuild and restart. The current app stays
open until the new build succeeds. Ctrl+C stops the app and runner. There is no
hot reload or automatic rebuild on save.

On macOS, the command packages and launches `Monocode Native.app`. Other
platforms launch the executable directly.

## Build

```sh
npm run native:build
```

This creates an optimized build without launching it. **The release app uses the
installed MonoCode database, and changes save there.** No extra flags are needed.

On macOS, open:

```sh
open "crates/monocode-gpui/target/release/Monocode Native.app"
```

The app has a separate name and application ID (`dev.usemono.gpui`) so it can be
installed alongside MonoCode. Local source builds are signed locally, without
Apple notarization or configured automatic updates. Linux and Windows builds
produce a standalone executable
under `crates/monocode-gpui/target/release/`. `CARGO_TARGET_DIR` and
`CARGO_BUILD_TARGET` can change these output paths.

The **Monocode Native Preview** GitHub Actions workflow runs checks and build
verification.

## Data and configuration

Development stores `monocode.db` under:

| Platform | Development directory |
| --- | --- |
| macOS | `~/Library/Application Support/Monocode Development/` |
| Linux | `$XDG_DATA_HOME/Monocode Development/`, or `~/.local/share/Monocode Development/` |
| Windows | `%LOCALAPPDATA%\Monocode Development\` |

`MONOCODE_NATIVE_DATA_DIR` overrides this development directory.

Release builds open `com.monocode.desktop/monocode.db` under macOS Application
Support, Linux's XDG data directory, or Windows' `%APPDATA%`. The app uses the
existing desktop schema and session IDs. GPUI preferences are saved beside the
database in `monocode.db.gpui.sqlite3`.

Supported desktop preferences and workspace state are read from the installed
app's webview localStorage on macOS and Linux. The Windows WebView2 reader is
implemented and covered by synthetic fixtures on Linux; reading a real Windows
profile remains unverified. Native preference edits are stored separately.
Some desktop layout concepts still need native support; see
[preview limitations](parity.md).

The Windows reader always opens a temporary copy of localStorage. Copying while
Tauri is writing is best-effort: an inconsistent copy can leave desktop
preferences incomplete or unavailable for that launch. Close Tauri before
starting GPUI for the first time. See [verification](verification.md) for
recorded checks.

To inspect the installed database without opening a window or changing it, use
the executable from a release build:

```sh
crates/monocode-gpui/target/release/monocode-gpui --check-data
```

On Windows, the terminal prefers PowerShell 7 (`pwsh.exe`), then Windows
PowerShell (`powershell.exe`), with `COMSPEC`/`cmd.exe` as the fallback. A `SHELL`
environment variable pointing to an installed native executable overrides this
order. Missing executables and Unix-only shell paths are ignored.

Provider CLI paths can be changed in Settings → Providers. CLI authentication
continues to use each provider's existing sign-in.

## Checks and fixtures

```sh
npm run native:check
```

This runs the script tests, Rust formatting, strict Clippy and native tests.
See [verification](verification.md) for individual commands, optional provider
checks and the latest recorded results.

A synthetic long-history fixture uses development data and makes no provider
request:

```sh
MONOCODE_NATIVE_DATA_DIR=/tmp/monocode-native-lab npm run native:dev -- --fixture
```

Fixtures are refused with production data. Use a fresh temporary directory when
you want an empty fixture store.

## Contributing

See [architecture](migration.md) for the crate boundaries and renderer patches,
and [preview limitations](parity.md) for remaining work. The
[session visuals guide](visuals.md) documents the agent-facing visual API and
examples.
