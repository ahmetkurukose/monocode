# Project rail: tree view — implementation plan

An optional second arrangement of the project rail (`crates/monocode-gpui/src/shell/project_rail.rs`):
each project row can expand to list its sessions (status dot, title, model, elapsed) and
its existing terminals, in one compact tree, so the rail wastes less space and "where did I open
that terminal in socradar" is one glance and one click away. Modelled on Orca's sidebar.

Status: plan, revision 2 (simplified after review). No code. Target branch `AK/monocode_enhancements`.

## 1. Goals and non-goals

Goals
- A Settings preference "Project list: List / Tree". Default `List` = today's rail, pixel for pixel.
- Tree: `▸/▾` per project; children are the project's sessions and its existing terminals, compact rows.
- Clicking a child switches project and opens that session / focuses that terminal.
- Small enough to carry on top of upstream (see §10): few hooks in upstream files, the rest in a new file.

Non-goals (cut in revision 2)
- No third layout value. `classic_layout` / `new_layout_boxy` (workspace.rs:137-139) stay as they are.
- No change to how terminals are created, kept alive or restored. The tree only *reads* what exists
  (§2.3); terminal lifecycle is independent of this feature and of the layout.
- No foreground polling for inactive projects (`poll_foreground`), no "not started" terminal rows.
- No "only active" filter, no heading toggle button, no collapsed badges, no compact-rail dot.
  These can come later as separate small commits if wanted.
- No change to the session sidebar, the Focused layout or the icon-only rail.
- No keyboard navigation inside the rail (none exists today).

### Layout naming (settled)
The rail is drawn only when `workspace.classic_layout` is true (`project_rail_drawn_width`,
project_rail.rs:147-155); Settings labels that state **Classic** (appearance.rs:284-287). The feature
targets Classic. The setting is shown only for Classic (`ctx.classic`).

## 2. Findings (verified in code)

### 2.1 How the rail is built and virtualized
- The rail is a GPUI `list()` with `ListState::new(0, ListAlignment::Top, px(96.))`
  (project_rail.rs:34, 303) — **variable-height rows already**, not `uniform_list`. The session sidebar
  is the one with a uniform 33 px height (sidebar_list.rs:52). Row heights already vary: the Monos
  roster (remeasured at :86-88), 30 px headings, 32/33 px project rows, 8 px gaps.
- Rows are a flat `Vec<ProjectRailRow>` (`Monos | Heading | Group | Project{path,grouped} | Gap | Empty`,
  :14-21) rebuilt on **every** rail render by `prepare_project_rail` (:333-405, called at :301), then
  diffed prefix/suffix and `splice`d into the `ListState` so the scroll anchor survives (:60-89).
  `compact` (icon-only rail) returns before the list is built (:276-299) — the tree never applies there.
- Row rendering: `project_rail_list_row` (:406-440) → `project_rail_row` (:1153-1389). The row is
  `h(32|33)`, click → `select_project` (:1355-1360), left mouse-down → `start_project_rail_drag`
  (:1361-1366), right → `MenuTarget::Project{rail:true}` (:1367-1381). Diff stats are queued per visible
  row (`queue_project_rail_stats`, :1742) and loaded in one deferred background batch (:1762-1787).
- Group headers (:906-1083) are the in-house precedent for a disclosure row: chevron-right/down, 
  `aria_expanded`, click toggles `group.collapsed`, then `persist` + `projects_frame.notify` (:1061-1075).
- Drag reorder: on press, `start_project_rail_drag` reads every **peer's** on-screen bounds from
  `AnchorSlot` probes (`project_rail_list.bounds[path]`, `.group_bounds[id]`); a group's block is
  stretched down to its last member row (:1530-1545). Moves animate via `TabShift` offsets keyed by
  `rail_shift_key` (:1448-1472); the dragged row is `deferred` on top (:1384-1388); members follow their
  group's offset (:1461-1472). Peers = same pinned state and same group (`project_rail_peers`,
  workspace.rs:635-639).
- The rail lives inside a cached `Chrome` entity (`projects_frame`, shell.rs:160-195;
  `.cached(...)` at project_rail.rs:193-195). It only redraws when something calls
  `projects_frame.update(cx, |_, cx| cx.notify())`; `invalidate_chrome` does so only under
  `classic_layout` (shell.rs:1650-1656). `notify_sidebar` (shell.rs:1646) does **not** touch it.
- `project_rail_virtualizes_projects_without_loading_their_sessions` (:2031-2055): adds 1000 projects,
  switches to the rail, asserts (a) `rail-project-999` has no bounds — off-screen rows are never
  constructed — and (b) `shell.sessions.len()` is unchanged. `shell.sessions` is the map of
  **`Conversation` entities** (shell.rs:670); the invariant is "rendering the rail never materializes
  a chat". The tree keeps it by reading only `session_summaries` / `session_metadata`
  (session_loading.rs:28-38) and never calling `ensure_session`.

### 2.2 Sessions per project, status, elapsed, model — what is cheap
- All sessions of all projects are in memory from startup: `session_summaries: HashMap<i64, Session>`
  and `order` (shell.rs:1307-1317; `Session` = id/title/provider/cwd/has_messages/updated_at/
  archived/pinned, store.rs:30-42). Per-project filtering is a scan on `summary.cwd == path`
  (precedent `project_sessions`, project_removal.rs:153-159; the sidebar's `sidebar_session_ids`,
  shell.rs:6742-6790, adds `has_messages`, Mono-hidden, orchestration-worker and worktree filters).
- `session_metadata(id)` (session_loading.rs:28-38) gives `busy`, `responding()`, `needs_approval()`,
  `display_title()` without loading anything; it reads the `Conversation` only if already loaded.
  Running sessions are always loaded (they run in-process), so running/done status for **any** project
  is cheap: `busy_sessions`, `unseen_finished` (maintained by `refresh_session_mark`, shell.rs:1497-1536).
- The "Working" card (live_agents.rs:55-90) is the data source to reuse: it walks `order`, keeps
  `responding() || (done && unseen_finished)`, reads `transcript.live_turn(cwd)` for the current tool,
  `started_at`, `duration_ms` (:65-66), sorts approvals → working → done. `live_elapsed` (:24-43) and
  `next_label_change` (:47-52) drive the per-second tick (:123-145). The tick calls `notify_sidebar`
  (:137) — in the Classic layout the card is rendered inside the rail (project_rail.rs:328), so the
  tree's elapsed labels need the tick to also notify `projects_frame`.
- Model label precedent (rails.rs:319-330): live `Conversation::sidebar_details()` else
  `pickers::cached_model_name(Kind::from_id(provider), session_details[id].model)`.
  `session_details` is loaded once at startup (shell.rs:1162, `MobileSessionMetadata.model`,
  store/session_metadata.rs:21-27) — no per-row I/O.
- Idle rows show `monocode_core::time::relative(updated_at, now)` like `classic_session_status`
  (rails.rs:663-713: Need approval / Working... / Done / Draft / relative time).

### 2.3 Terminals — enumerable, with two caveats
Two kinds, both enumerable for every project:
- **Dock terminals** are per project: `workspace.docks: HashMap<String, TerminalDock>` keyed by project
  path (workspace.rs:130-131); `TerminalDock.terminals: Vec<String>` is the persisted cwd list
  (:234-241). Live shells: `Shell.terminals: HashMap<String, Vec<Entity<Terminal>>>` (shell.rs:820-821),
  same key, same index. Shells survive project switches (the map is never pruned on switch) but are
  **spawned lazily** — `ensure_terminals(project)` (shell.rs:3226-3242) runs only for the current
  project when its dock is shown or a terminal is added (:3297, :3350, :3546). After a restart, an
  inactive project has cwd strings but no `Terminal` entity until you visit it.
- **Pane / title-tab terminals**: `workspace.terminal_panes: HashMap<i64, TerminalPane{owner, cwd}>`
  (workspace.rs:222-228) with live views in `terminal_pane_views` (terminal_panes.rs:83-95, also lazy).
  Their project is `session_summaries[owner].cwd`. Title: `terminal_pane_title(pane)` (:97-106).
- `Terminal` exposes `cwd`, `title` (foreground command or directory), `foreground: Option<String>`,
  `exited`, `visible` (terminal.rs:119-146). Caveat 1: the 1 s foreground poll is gated on `visible`
  (terminal.rs:292-300), and `with_dock` sets `visible = (path == current project && !app_view_open)`
  for every live terminal (shell.rs:3514-3521). So `foreground`/`title` of inactive projects are
  **stale** (frozen at the moment you left). `visible` has no other consumer except selection
  (terminal.rs:628), so letting expanded projects keep polling is safe (§4.5).
- Focusing: `open_terminal_pane(pane)` (terminal_panes.rs:272-305) already works cross-project — it
  sets `workspace.project` from the owner session (:284-286), opens the tab and focuses the shell.
  Dock focusing is current-project only (`select_terminal`, shell.rs:3359-3367; `toggle_running_terminal`,
  terminal_dock.rs:298-306; `focus_terminal`, shell.rs:3250-3258) — a small wrapper is needed (§4.6).
  Caveat 2: when `terminal_placement() == "panel"` the dock's shells show in the right panel
  (`show_panel_terminal`, terminal_dock.rs:235-248); the wrapper must branch on `terminal_in_panel`.
- Remote projects have no terminals (shell.rs:3227, :3280).

### 2.4 Navigation functions
- Project row click: `select_project(path)` (shell.rs:4556-4642) — restores the project's remembered
  tab/session or creates a blank one.
- Sidebar session click: `select_sidebar_session` (session_groups.rs:378-418) → `Shell::open(id)`
  (shell.rs:4337-4356) → `activate_session` (:4400-4442), which sets `workspace.project` from the
  session's cwd (:4425). So `open(id)` on a session of another project already switches project.
  `open` also routes pane ids to `open_file_pane` / `open_mono_pane` / `open_terminal_pane` (:4338-4349).
- Title tab click: `open_title_tab` (title_tabs.rs:182-214) → `open(tab_focus)`.

### 2.5 Preferences end-to-end
- `Preferences` (settings/preferences.rs:9-112, `#[serde(default)]`), defaults :118-196,
  `restore_appearance` :242-264. The old `collapsed_project_rail` is retired: `Workspace::restore`
  forces it to "compact" (workspace.rs:466-467; test :1287-1300).
- Page widgets: `segmented` (settings/widgets.rs:405; debug selector `settings-{id}-{option}`),
  used for the Layout and Workspace-style rows (appearance.rs:276-349). A row writes with
  `this.update(cx, |p| ...)`, which emits `SettingsEvent::Changed(Box<Preferences>)` (settings/mod.rs:115),
  handled at shell/settings.rs:170-183: copy into `workspace.preferences`, `apply_preferences`,
  `persist`, `invalidate_chrome` (→ `projects_frame` redraw under `classic_layout`).
- `SettingsContext.classic` (settings/mod.rs:53-58) is the gate for Classic-only rows.

### 2.6 Persistence, a11y, styling precedents
- `Workspace` is `#[serde(default)]` (workspace.rs:69-70), saved by `persist` → `save_state`
  (shell.rs:4230-4262). Per-item UI state already lives there (`project_rail_groups[].collapsed`,
  `project_rail_pinned`). Restore migrations go in `Workspace::restore` (:440-472).
- `Role` is `accesskit::Role` (vendor/gpui-pre/src/elements/div.rs:1310) → `Role::Tree`, `Role::TreeItem`
  exist; `aria_level` (:1457), `aria_expanded` (:1400), `aria_selected` (:1394),
  `aria_position_in_set` (:1462) are available.
- Theme helpers in use: `content(α)`, `accent_color()`, `pane_fill()`, `tabular()`, `icon(name, size)`,
  `rgb(AMBER)` / `rgb(EMERALD)` (live_agents.rs:8-9), `icon("terminal", 14.)` (terminal_panes.rs:27).

### 2.7 Tests
- Shell fixture `shell::tests::window` (shell.rs:12212-12255): projects `/work/another-project`
  (session `a`, codex, has messages) and `/work/monocode` (session `b`, claude); returns
  `(window, a, b)`. Rail tests switch with `shell.toggle_layout(cx)` or set `classic_layout = true;
  project_rail_open = true` (project_rail.rs:1897-1926), use `VisualTestContext::debug_bounds`,
  `simulate_click/mouse_*`, `cx.set_reduce_motion(true)`.
- Settings page tests: settings/layout_tests.rs:38-62 (toggle `view.ctx.classic`, assert
  `settings-new-layout-style-*` presence).
- Terminal fixtures: `dock_with_one_terminal`, `title_terminal` (terminal_panes.rs:367, :412).
- Live: `cargo build --manifest-path crates/monocode-gpui/Cargo.toml --locked -p monocode-gpui --profile native-dev`,
  then `target/native-dev/monocode-gpui --dev-data --cwd PATH` (as scripts/perf/frame-run.sh:35),
  `screencapture` of the window.

## 3. Decisions

| Decision | Choice | Why |
|---|---|---|
| Where the switch lives | `Preferences.project_rail_view: String` = `"list"` (default) / `"tree"` | Same family as `collapsed_project_rail`; follows `restore_appearance`; one `SettingsEvent::Changed` path. Avoids the 134-site layout refactor. |
| Settings placement | Appearance → Layout group, row "Project list", only when `ctx.classic` | Mirrors how "Workspace style" is shown only for the other layout. |
| Expansion state | `Workspace.project_rail_expanded: HashSet<String>` (persisted) | Per-workspace UI state like `project_rail_groups[].collapsed`. |
| Row model | Extend `ProjectRailRow` with `Session`, `Terminal`, `More`; one flat `Vec` | Keeps the existing `list()`, splice diff and drag code; variable heights already work. |
| Session source | `session_summaries` filtered by `cwd == path`; status from `busy_sessions` / `unseen_finished` / `session_metadata`; elapsed from a loaded running `Conversation`'s `live_turn` | Zero I/O, never calls `ensure_session`; keeps the virtualization invariant. |
| Terminal source | Only terminals that exist now: `Shell.terminals[path]` (dock) and `workspace.terminal_panes` grouped by owner cwd | Read-only; no spawning, no polling. After a restart, projects not visited yet simply show no terminal rows until their shells exist. |
| Terminal label | The terminal's last known title; no live "running" marker | Avoids the visibility-gated poll entirely. |
| Children cap | 8 sessions per project (most recent first), then a "N more" row that selects the project | Rail stays short; running work is never hidden. |
| Click semantics | Chevron: toggle only. Project row: `select_project` (and expand, tree mode). Child: open / focus. | Keeps today's row click. |
| Where the code lives | New `shell/project_rail_tree.rs` (child rows, builders, navigation); upstream files get only thin hooks | Merge-friendliness, §10. |

## 4. Design

### 4.1 Preference and settings
- `preferences.rs`: `pub project_rail_view: String`, default `"list"`, reset in `restore_appearance`.
  Helper `fn project_rail_tree(&self) -> bool` (unknown values fall back to List).
- `appearance.rs`: when `self.ctx.classic`, one `segmented` row "Project list" with List / Tree.
- No migration: `#[serde(default)]` gives `"list"` to existing files. A `restore` test pins it.

### 4.2 Row model
```rust
enum ProjectRailRow {
    Monos, Heading(&'static str), Group(String),
    Project { path: String, grouped: bool },
    // tree mode only
    Session  { project: String, id: i64, last: bool },
    Terminal { project: String, source: RailTerminal, last: bool },
    More     { project: String, hidden: usize, last: bool },
    Gap, Empty,
}
enum RailTerminal { Dock(usize), Pane(i64) }
```
`last` marks the final child so it carries the 1 px row gap that `project_rail_row` puts before the next
project (`row_gap`, :415-418, :1205-1208).

### 4.3 Building rows
After each `Project{path}` push in `prepare_project_rail`, if the tree is on, the project is in
`project_rail_expanded` and the rail is not compact: extend with `self.project_rail_children(path)`
(defined in the new file):
1. Sessions: `session_summaries` with `cwd == path`, not archived, has messages, minus Mono-hidden and
   orchestration workers (same predicates as `sidebar_session_ids`, shell.rs:6750-6759). Sort
   `Reverse(updated_at)`; first 8, then `More`.
2. Terminals: dock entries `0..Shell.terminals[path].len()` as `Terminal{Dock(ix)}`, then terminal panes
   owned by a session with `cwd == path` as `Terminal{Pane(id)}`.
Cost is O(sessions) per expanded project per render; cache behind a dirty flag only if the frame log
asks for it. Child rows are built only when `list()` requests them, so off-screen projects cost nothing
and `shell.sessions` is untouched (virtualization test, §6).

### 4.4 Child rows
- Session row: ~24 px, indented under the project label, 12 px text, truncated title and a trailing muted
  relative time. **No status dot, no elapsed time, no live tick**: running work is already shown by the
  sidebar's "Working" card (live_agents.rs), so the tree adds no cost for it. `Role::TreeItem`,
  `aria_level(2)`, `debug_selector("rail-session-{id}")`. Click → `self.open(id, window, cx)`.
- Terminal row: same frame, `terminal` icon, title. `debug_selector("rail-terminal-{project}-{n}")`.
- `More` row: "N more sessions…", click → `select_project(path)`.
- No per-row entities (`cx.new`).

### 4.5 Redraw
Structure changes already reach `projects_frame` via `invalidate_chrome` on `SessionChanged`
(shell.rs:1427-1449). Nothing time-based is drawn, so no new tick. Terminal title/exit events notify
`projects_frame` only when the tree is on (shell.rs:3221-3222).

### 4.6 Navigation
- Session: `open(id)`. Pane terminal: `open_terminal_pane(pane)` (already cross-project).
- Dock terminal: new `focus_dock_terminal(path, ix, window, cx)` in the new file: `select_project(path)`
  if needed; `ensure_terminals(path)`; set `dock.active = ix; dock.open = true` (or
  `show_panel_terminal` when `terminal_in_panel`); `slide_dock(true)`; scroll tab into view;
  `focus_terminal`; `persist`. Composed from `toggle_running_terminal` (terminal_dock.rs:298-306) and
  `select_terminal` (shell.rs:3359-3367).
- `reveal_active_project_rail` (:1390-1396) also reveals the active `Session` row when expanded.

### 4.7 Drag, pinned, groups
- Project ordering is unchanged (`project_rail_peers`). In `start_project_rail_drag` a project's block is
  stretched to its last child's bottom exactly as a group's is (:1530-1545).
- Children apply the parent's drag offset and `deferred` raise (:1384-1388) so they travel with it and
  never start a drag themselves. Pinned and grouped projects expand the same way.

### 4.8 Keyboard and a11y
Tree container `Role::Tree`; project rows `Role::TreeItem`, `aria_level(1)`, `aria_expanded`; children
level 2. Keyboard navigation is out of scope.

## 5. Work packages

Each is one commit; build, clippy (`-D warnings`) and the touched tests must pass.

| # | Package | Output | Estimate |
|---|---|---|---|
| 1 | Preference + Appearance row + `restore` test | Setting visible and persisted; rail unchanged | ¼ session |
| 2 | `project_rail_expanded`, chevron, `Session`/`More` rows, rendering, click, redraw; virtualization test in tree mode; drag block with children | Sessions under projects, cross-project open | 1–1½ sessions |
| 3 | `Terminal` rows (dock + pane), `focus_dock_terminal` | Existing terminals under projects, click focuses | ¾ session |

Total ≈ 2–2½ sessions. Packages 1–2 alone already deliver the compact layout; package 3 is the
terminal-finding part and can ship separately.

## 6. Test plan

monocode-gpui, `#[gpui::test]` unless noted:
- (unit) Preferences from `"{}"` → `"list"`; `restore_appearance` resets; `Workspace::restore` of a
  pre-feature state keeps List and an empty `project_rail_expanded`.
- Settings: the Tree option has bounds only when `ctx.classic` (pattern of layout_tests.rs:39-62).
- Virtualization: existing test re-run in tree mode with every project expanded: `rail-project-999`
  absent, `rail-session-*` only for visible projects, `shell.sessions.len()` unchanged.
- Layout: Classic + Tree, expand `/work/another-project`: `rail-session-{a}` is indented
  (`left()` > project button's) and below the project row; absent for a collapsed project; chevron
  toggles and `ui_state` round-trips (as :1985-1987).
- Navigation: from `/work/monocode`, click `rail-session-{a}` → `workspace.project` is the other project,
  `workspace.active == Some(a)`, no extra `Conversation` beyond `a`.
- Drag: `original_project_rail_reorders_groups_and_projects` variant with the dragged project expanded:
  children move by the same delta and the order persists.
- Terminals: `dock_with_one_terminal` on project B while A is current → `rail-terminal-B-0` under B;
  click → `workspace.project == B`, `docks[B].active == 0`; a `title_terminal` pane → click opens it.
- Live check: native-dev build with `--dev-data --cwd`, a few real projects, Classic + Tree, terminals
  in two projects, `screencapture` the result.

## 7. Risks

- **Projects not visited this run have no terminal entities** after a restart, so no terminal rows until
  visited. Accepted (revision 2): we only show what exists. Eager spawning is not proposed.
- **Rail redraw**: elapsed labels redraw the cached `projects_frame` once per second while anything runs
  (the sidebar already does). Child rows stay entity-free.
- **Drag measurement**: if the last child is scrolled off-screen the block falls back to row-only height
  (same limitation groups have today).
- **Upstream drift** in the rail files: see §10.

## 8. Open questions (recommended answers in bold)

- Q1. Project row click in tree mode: select only, or select + expand? **Select + expand; chevron toggles only.**
  Auto-collapse the previously active project? **No.**
- Q2. 8 sessions per project plus "N more"? **Yes.**
- Q3. Should child rows get a right-click menu? **No in v1 (sidebar menu may assume the active project).**
- Q4. Ship order: 1+2 first (compact tree), 3 (terminals) after? **Yes.**

## 9. Not verified

- Whether the sidebar's session context menu assumes the active project.
- Whether the "Working" card's elapsed labels advance in Classic (the tick notifies only `sidebar_frame`,
  shell.rs:1646); §4.5 would fix that as a side effect.
- Exact pixel values of the child indent; tuned against a screenshot in package 2.

## 10. Upstream merge impact

How we carry the fork: `scripts/sync-upstream.sh --rebase` rebases our commits (47 fork-only today) onto
the newest upstream `native-v*` tag. A rebase replays every commit of ours, so what hurts is *our* commits
that edit lines upstream also edits.

Measured today (upstream/main, last 60 days, ~1200 upstream commits in total): upstream touched
`project_rail.rs` 8 times, `sidebar_list.rs` 10, `session_groups.rs` 10, `rails.rs` 4. We touched
`project_rail.rs`/`sidebar_list.rs` only twice so far (Database rail entry, working card). So the rail is a
**moderately active** upstream area, not a quiet one.

What this feature touches in upstream-owned files, and how we keep it thin:

| File | Our change | Conflict exposure |
|---|---|---|
| `project_rail.rs` | 3 extra `ProjectRailRow` variants; a chevron on the project row; a call to `project_rail_children` in `prepare_project_rail`; drag block stretch; reveal | **Medium** — the only real hot spot (≈5 hunks). Mitigation: keep each hook to a few lines and put the logic in `project_rail_tree.rs`. |
| `workspace.rs` | one `HashSet` field | Low (additive, like our other fields) |
| `preferences.rs`, `appearance.rs`, `shell/settings.rs` | one field and one row | Low (additive) |
| `shell.rs`, `live_agents.rs` | two `notify` lines | Low |
| `terminal_dock.rs` / new file | `focus_dock_terminal` lives in the new file | None |

Verdict:
- **Merging stays workable**, but unlike the Database view (almost entirely new files) this feature
  does edit a file upstream changes about once a week. Expect an occasional conflict in
  `project_rail.rs` at each sync; with thin hooks it is a few minutes of work, not a rewrite.
- **Compile-time breakage** is the more likely cost: if upstream adds a `match` over `ProjectRailRow`,
  our new variants make it non-exhaustive and the build fails until we add arms. It is loud and easy
  to fix, never silent.
- **Can it stop us from taking an upstream feature?** Only in one scenario: upstream restructures or
  rewrites the rail (or retires the Classic layout, which is what the rail belongs to). Then either we
  re-port our ~300-line tree on top of the new rail, or we drop the tree and keep the rest. We never lose
  upstream's feature because of this; the worst case is losing *ours*. The default `List` setting means
  upstream's behaviour is also what we ship unless the user opts in.
- **Cheap insurance:** (1) ship as two or three small commits so a conflict only replays a small
  patch; (2) keep all logic in `project_rail_tree.rs`; (3) keep tests of tree behaviour in that file's
  own test module, so an upstream rail refactor breaks one test file, not the existing rail tests;
  (4) send it upstream as a PR if the maintainer wants it — a feature upstream owns is no longer a
  merge cost for us. (Upstream's own roadmap on Classic should be checked before starting:
  if Classic is being phased out, the whole feature is poor value.)

Upstream's stance on Classic (checked in `git log upstream/main`): the original rail layout was
deliberately restored (`02cc783d`, "Restore original project rail navigation layout") and then renamed
"Classic" next to "Focused" (`444f9b4d`), with Focused as the default for new users. So Classic is a
kept, maintained option, not a deprecated one, but it is the secondary layout; there is no sign it is
being retired. Re-check `git log upstream/main -- crates/monocode-gpui/src/shell/project_rail.rs` right
before starting.
