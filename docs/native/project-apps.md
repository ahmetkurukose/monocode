# Project apps: Inbox, Notes and Database as project tabs — plan

Status: plan, revision 2 (stress-tested against the code, 2026-11-10). No code yet. Branch
`AK/monocode_enhancements`, on top of `native-v0.16.0-preview.14` (60 fork commits).
Line numbers are from that tree.

## 1. Goal

In **Classic + Tree** (Settings → Appearance → Project list: Tree):

- Every expanded project in the rail tree gets a one-line row of small icons — **Inbox,
  Notes, Database** — as its last child. Each icon can be hidden in Settings.
- Clicking one switches to that project and opens the app as a **tab in the title strip**,
  the way Focused already shows app tabs.
- The tab **belongs to the project**: it shows in that project's strip only, one tab per
  (app, project).
- Each app shows that project's slice: the project's Notes, the Inbox scoped to it, and the
  Database's data sources assigned to it. Data-source assignment is part of this release.

Decisions already taken (not reopened here): Classic + Tree is the layout; one row of icons
with per-icon visibility; tabs are project-scoped; app views stay singleton entities that are
re-scoped per tab (no entity per project, no split panes); Automations stays global.

### Non-goals

- No change to **List** mode or to Focused. Everything is gated on `project_rail_tree_on()`
  (`shell/project_rail_tree.rs:16`: Classic and `project_rail_view == "tree"`), so
  upstream's Classic surfaces and the 17 tests in `shell/classic_surface_tests.rs` keep
  their behaviour.
- No app view beside a session. An app view is still a *mode* that replaces the workspace
  body (`active_app_view`, `app_view_body`, `shell/app_views.rs:8-9, 219-226`).
- No worktree scoping of app tabs: a project's app tab shows in every worktree of it.

## 2. What exists today (verified)

### 2.1 App views are a mode, Focused shows them as tabs, Classic as surfaces
- `Shell.active_app_view: Option<AppView>` (`shell.rs:729`) is the only state. The entities
  are singletons (`self.inbox / notes / database / automations`), made lazily by
  `open_inbox / open_notes / open_database / open_automations` (`app_views.rs:244-418`).
  Each `open_*` first calls `prepare_app_view` (`:187-200`: leaves search, closes preview,
  menu, Settings, Monos), re-feeds the view with `workspace.project` + `projects`
  (`set_context`, `:273`, `:334`, `:385`), then `show_app_view` (`:202-217`).
- `show_app_view` only records a tab outside Classic: `if !classic_layout {
  remember_title_tab(TitleTab::App(view)); reveal_active_tab }` (`:207-210`).
  `leave_app_views` (`:153-161`) flushes Notes and clears the mode; `close_app_view`
  (`:163-185`) additionally returns to the project's remembered session.
- `TitleTab` (`workspace.rs:37-46`): `Session(i64) | Search | App(AppView) | Mono{id,
  project, worktree}`. Saved inside `Workspace.title_order`, `pinned_tabs`, `scope_focus`
  (`:74-78`) and `mono_panes` (`:121`).
- `scoped_tabs` (`shell/title_tabs.rs:80-120`) hides `App` in Classic (`:86-89`) and shows it
  in *every* project's strip in Focused (it is global). `tab_scope()` = `project\0worktree`
  (`:45-51`) keys `scope_focus`; `restore_scope_tab` (`:216-230`) reopens a scope's tab if
  it is in `visible_tabs`.
- Every project switch passes through `leave_app_views`: `select_project` (`shell.rs:4696`)
  and `activate_session` (`shell.rs`, `fn activate_session`, before it assigns
  `workspace.project` from the session's cwd). So **while an app view is open,
  `workspace.project` is the project it was opened from** — the invariant §3.2 builds on.
- `select_project` in Classic only restores a remembered tab when it is a `Session`
  (`shell.rs:4714-4723`); otherwise it falls back to `project_focus`.
- `active_app_view` has 21 sites, `app_view_open()` 24 (plus tests). Only these need to
  know *which project* an app tab belongs to: `app_title_tab` (`app_views.rs:27`, selected
  state), `active_title_tab` (`title_tabs.rs:123-125`), `finish_close_title_tabs`
  (`:357-361`), ⌘W (`shell.rs:9255-9261`), `layout_changed` (`shell/layout.rs:186-190`),
  the startup reopen in `automation_runs.rs:101-107`, and the three Classic layout gates in
  §2.3. Every other site asks "is the body covered?" and stays as it is.

### 2.2 The title strip
- Rendering: `shell.rs:8540-8560` matches `TitleTab` and calls `app_title_tab(view, width)`
  (`app_views.rs:21-132`, selector `app-title-tab-{label}`, close button, drag start, right
  click → `MenuTarget::AppTab(view)` (`shell.rs:377`), middle click, click → `open_title_tab`).
- Non-wildcard matches on `TitleTab` the compiler will flag when a variant is added:
  `TitleTab::session` (`workspace.rs:49-54`), `scoped_tabs`, `open_title_tab`
  (`title_tabs.rs:188-213`), the tab-menu target and label (`shell.rs:5704-5710`, `:5799-5808`),
  `start_tab_drag`'s `DragSource` (`shell.rs:7990-7994`), the strip renderer (`:8540`),
  `tab_menu_target` and `pin_card` (`shell/pinned_tabs.rs:107-112`, `:138-157`).
- Pinning: `pinned_tabs: Vec<TitleTab>`, `toggle_pin_tab` (`pinned_tabs.rs:87-105`),
  `prune_pinned_tabs` (`workspace.rs:1168-1174`) — generic over the variant.
- Reorder: `reorder_title_tab` (`title_tabs.rs:405-425`) rewrites `title_order` and rebuilds
  `tabs` from `TitleTab::session` — generic.
- Close: `close_title_tabs` → `finish_close_title_tabs` (`title_tabs.rs:286-402`) picks the
  neighbour, leaves the app view if the closing tab is the active one (`:357-361`), prunes
  `title_order`, `scope_focus`, pins.
- `Workspace::restore` (`workspace.rs:443-622`) rewrites `Session` keys in `title_order`,
  `scope_focus`, `pinned_tabs` (`:518-527`), then `normalize_title_order` (`:1153-1166`)
  which keeps every non-session tab and dedups.
- **Serde.** `Workspace` is `#[serde(default)]` without `deny_unknown_fields`
  (`workspace.rs:69-70`; the only `deny_unknown_fields` in the tree is
  `monocode-core/src/visual/model.rs:8`), so an *unknown field* is ignored by older builds.
  `TitleTab` is a plain externally-tagged enum: an *unknown variant* makes
  `serde_json::from_str::<Workspace>` fail, and `restore` then does `.ok().unwrap_or_default()`
  (`:444-446`) — **the whole saved workspace is replaced by `Workspace::default()`**:
  projects, groups, pins, docks, actions, right panel, and `preferences` (which live inside
  `Workspace`, `:153`) are all lost, and the next `persist` writes that empty state over the
  old one. There is no backup file (`save_state`, `shell.rs:4311-4321` → `store.save_ui_state`).
  The draft's "accepted for a local build" is not acceptable: any older build of the fork
  (e.g. a `backup/before-*` branch after a bad rebase) or an upstream build wipes the
  user's setup. §3.5 avoids the new on-disk variant entirely.

### 2.3 Classic body and layout
- `classic_surface_open` (`shell/classic_surfaces.rs:5-11`) = Classic ∧ (Settings ∨ app
  view ∨ search page ∨ Mono). It drives: the footer (`layout.rs:367`,
  `(!classic_surface_open).then(footer_row)`), `compact_surface` (`:370-374`, macOS, rail
  closed), and `session_sidebar_available` (`classic_surfaces.rs:13-16`), which the tree
  already turns off.
- `classic_body` (`layout.rs:345-510`) picks the tab bar: `spans_window || search → None;
  original && app_view_open() → classic_app_surface_header; original && mono →
  classic_mono_header; else titlebar` (`:375-384`). The surface header
  (`classic_surfaces.rs:18-63`, selector `classic-app-surface-header`) is what the 17
  Classic tests assert on (`classic_surface_tests.rs:253-262`).
- `classic_titlebar_spans_window` (`layout.rs:151-159`): macOS ∧ Classic ∧ rail closed ∧
  no Settings ∧ `!app_view_open()` ∧ no search ∧ no Mono → the strip spans the window above
  the rail. With an app tab open this must behave like a session, or the strip jumps.
- The corner panel toggle is already drawn in tree mode (`layout.rs:387-388`,
  `!original || project_rail_tree_on()`), and `right_panel_shown` / `sync_right_panel`
  already allow the right panel in tree mode (`shell.rs:1718-1740`) but not under an app
  view (`!app_view_open()`). `set_right_panel_open` refuses under an app view (`:1799`).
- Footer contents under an app view: `terminal_footer` returns `None` (`terminal_dock.rs:983`),
  `pressure_chip` goes "alone" (`layout.rs:836`), the dock is not drawn (`shell.rs:3594`),
  and a new terminal first closes the app view (`shell.rs:3339`). So a footer under an app
  tab shows usage + branch only. Acceptable; nothing else to change.
- The back arrow in Classic runs `classic_rail_back` (`classic_surfaces.rs:131-151`): closes
  Settings, else a Mono, else `visit(-1)`. Unchanged; an app tab is closed with its ×, ⌘W
  or Esc (`shell.rs:9388`, `close_app_view`).
- Rail navigation rows: the full rail has Inbox / Notes / Automations / Database rows
  (`project_rail.rs:668-727`, selectors `sidebar-navigation-{label}`, hidden per row by
  `workspace.sidebar_*_visible`), the icon rail has the same four buttons (`:889-958`,
  `rail-inbox` …). Both call `open_*`. The unread dot reads `inbox.read(cx).unread_count()`
  (`:687-691`, `:733-736`), which counts every project's items (`inbox.rs:668-674`).
- The rail is a cached frame (`projects_frame`); it redraws on `invalidate_chrome` in Classic
  (`shell.rs:1664-1670`), which `show_app_view`, `persist` and the Settings `Changed` path
  (`shell/settings.rs:170-183`) all call.

### 2.4 The tree
- Child rows are built in `project_rail_children` (`project_rail_tree.rs:33-110`): sessions,
  `More`, terminals; `last` marks the final child. Rows are 30 px (`CHILD_HEIGHT`, `:12`),
  not 28. `project_rail_child_frame` (`:114-144`) puts the drag-measure probe
  (`project_rail_list.child_bounds[project]`) on the `last` child; `start_project_rail_drag`
  stretches the project's block to that probe (`project_rail.rs:1655-1662`). So a new last
  row is measured for free.
- `project_rail_list_row` (`project_rail.rs:464-487`) routes `Session | Terminal | More` to
  `project_rail_child_row` (`project_rail_tree.rs:186-238`), which also handles the raised
  (dragged) state.
- Preferences: `project_rail_view` (`settings/preferences.rs:61`, default `:159`, helper
  `:204-206`, reset in `restore_appearance` `:272`), Appearance row only `if !inset`
  (`settings/pages/appearance.rs:324-345`; `inset = !ctx.classic`, `:90`), applied through
  `SettingsEvent::Changed` → `apply_preferences` + `persist` + `invalidate_chrome`
  (`shell/settings.rs:170-183`, `:347-400`). `layout_tests.rs:93-94` pins the selectors.

### 2.5 Per-app scoping today
- **Inbox**: `Inbox::set_context(projects, cwd)` (`inbox.rs:601-636`) sets
  `filters.hidden_projects` to the other projects *when the cwd changes* (`:610-615`), saves
  the filter, re-selects, refreshes. It is called on every `open_inbox` through
  `ensure_background_inbox` (`app_views.rs:329-335`) with `workspace.project`, and at
  automations start (`automation_runs.rs:90`, `:163`). `hidden_projects` is applied in
  `Filters::matches` by `item.project_path` (`monocode-core/src/inbox.rs:318-326`; tracker
  items — Linear, Jira — are exempt). **So the Inbox is already re-scoped on project
  switch in every layout**; the plan only adds a per-project unread count.
- **Notes**: `set_context(cwd, projects)` (`notes.rs:266-275`) sets `cwd`; `create` makes
  the note in `cwd` (`:482`); the list filters by the search query only (`:1330-1335`);
  `refresh` selects `notes.first()` when the selection is gone (`:311-318`).
  `Note::in_project(scope)` already exists in core (`monocode-core/src/notes.rs:159-165`):
  true for a note with no project (or a non-project cwd), else path-key equality.
  `leave_app_views` flushes drafts (`app_views.rs:154-158`); `refresh` flushes first (`:283`).
- **Database**: `DataSource` (`monocode-core/src/db/source.rs:19-37`, `#[serde(default)]`)
  has no project field; `datasources.json` is global. `DatabaseView` (`database.rs:496-542`)
  keeps `selected: Option<String>` (one source), `active: HashMap<source, console id>`,
  `open: HashMap<source, Vec<console id>>`, `expanded: HashSet<String>` (keys all start with
  the source id: `s:{id}`, `c:{id}`, `n:{id}/…`, `t:{id}/…`, `disconnect_source` `:2316-2323`),
  `consoles: Vec<Console>` keyed by `(source, id)` with each console's own `conn`, `running`,
  `task` (`:329-351`), `nodes: HashMap<source, SourceNode>` (live connection + schema tree).
  `rows()` (`:2400-2402`) iterates `self.sources`. `select_source` (`:2626-2631`) only sets
  `selected` and `ensure_consoles` (`:802-839`, idempotent per source). `remove_source`
  (`:2737-2757`) falls back to `sources.first()`. `SavedState` (`:53-69`, `#[serde(default)]`)
  persists `selected / active / open / expanded / send_target / sent`.
  **Everything but `selected` is already keyed by source id**, so two projects sharing a
  source share one console set, one connection, one `active` — which is the right
  behaviour (console files are never duplicated). Only `selected` is per *view*, and only
  it needs a per-project copy. A running query lives in `consoles[ix].task` and keeps
  running when the view re-scopes; it is only cancelled by `disconnect_source` (edit/remove).
- `send_targets(dir)` (`database/send.rs:21-38`) reads `datasources.json` straight from
  disk. Callers: `file_editor/selection.rs:182` (`FileEditor.cwd`, `file_editor.rs:139`),
  `explorer.rs:1057` (`Explorer.cwd`, `explorer.rs:189`), the tab menu `shell.rs:5748-5752`
  (`tab_file(id)`, `shell/database_send.rs:43`; the tab is in `workspace.project`).

## 3. Design

### 3.1 Tree row of app icons (rail)
- `ProjectRailRow::Apps { project: String, last: bool }` (`project_rail.rs:14-41`, additive).
  Appended by `project_rail_children` (`project_rail_tree.rs:33-110`) after terminals when
  `preferences.project_apps` is non-empty; `total += 1` so `last` lands on it.
- One `CHILD_HEIGHT` (30 px) row through `project_rail_child_frame` (keeps the drag probe and
  the row gap), indented like the other children, with up to three 24 px icon buttons:
  `inbox`, `note`, `database` (same glyphs as `AppView::icon`, `workspace.rs:25-32`).
  `Role::Button`, `aria_label("{App} of {project name}")`, selector
  `rail-app-{inbox|notes|database}-{rail index}` (as `rail-project-button-{ix}`).
- Selected state: `self.active_project_app_tab() == Some(ProjectApp{view, project})` (§3.2).
- Unread dot on Inbox: `inbox.read(cx).unread_count_in(&project) > 0` (new; `inbox.rs`
  next to `unread_count`, filtering `item.project_path == project`; items with an empty
  `project_path` count in no project's dot).
- Click: `open_project_app(view, project)` (new, in `project_rail_tree.rs`):
  `if workspace.project != project { select_project(project) }` then `open_inbox` /
  `open_notes` / `open_database`. `select_project` restores the project's remembered tab
  first (maybe a session); `open_*` then puts the app on top. No new `select_project` logic.
- Preference: `Preferences.project_apps: Vec<String>` (default `["inbox","notes","database"]`,
  reset in `restore_appearance`; unknown names ignored; empty = no row). Helper
  `project_app_shown(view)`. Appearance → Layout: a row "Project apps" with three `toggle`s,
  inserted after "Project list", shown only `if !inset && p.project_rail_tree()`.
  Hiding an icon hides the entry point only; already-open tabs stay.
- Redraw: `show_app_view` and `persist` call `invalidate_chrome` → `projects_frame`
  (`shell.rs:1664-1670`); nothing new. The rail nav rows (full and compact) are kept
  unchanged; see §3.3 for what they open in tree mode.

### 3.2 Tabs belong to the project
**Shape.** One new in-memory variant, never written to disk under its own name (§3.5):
```rust
// workspace.rs
pub(crate) enum TitleTab {
    Session(i64), Search, App(AppView), Mono { .. },
    /// Classic + Tree only: one app tab per project. Persisted through
    /// `Workspace::project_apps`, never serialized as this variant (see `saved()`).
    ProjectApp { view: AppView, project: String },
}
```
`remember_title_tab`'s `contains` check (`title_tabs.rs:152`) gives "one tab per
(app, project)" for free.

**Active tab.** No new `Shell` field. Because every project switch leaves the app view
first (§2.1), the active app tab is derived:
```rust
// shell/project_rail_tree.rs (or a new shell/project_apps.rs)
fn project_app_scope(&self, view: AppView) -> bool { self.project_rail_tree_on() && view != AppView::Automations }
pub(super) fn active_project_app_tab(&self) -> Option<TitleTab> {
    let view = self.active_app_view.filter(|v| self.project_app_scope(*v))?;
    Some(TitleTab::ProjectApp { view, project: self.workspace.project.clone() })
}
/// An app view drawn as a Classic surface (own header, no strip, no footer).
pub(super) fn classic_app_surface(&self) -> bool { self.app_view_open() && self.active_project_app_tab().is_none() }
```
This is safer than `active_app_project: Option<String>`: there is no second field to keep
in step, and flipping the tree preference or the layout re-derives the state (a global
`App` tab in Focused becomes a `ProjectApp` tab in Classic + Tree and back, which is what
`layout_changed` already does for the other direction, `layout.rs:186-190`).

**Entry points.** In tree mode *every* `open_inbox / open_notes / open_database` scopes to
the current project — the rail rows, the icon rail, the Inbox/Notes shortcuts
(`shell.rs:9155-9156`), the Settings "open inbox" link (`shell/settings.rs:151`), Quick Open,
all of them. The tree-row click only adds `select_project` first. So there is exactly one
code path, and the full-rail navigation rows stay as they are: in tree mode they open the
current project's tab (and show selected when that tab is active). Automations keeps
opening as a global surface in tree mode (`project_app_scope` is false for it).

**Sites, in order:**
- `show_app_view` (`app_views.rs:202-217`): after the Focused branch, `else if let
  Some(tab) = self.active_project_app_tab() { remember_title_tab(tab); reveal_active_tab }`.
- `active_title_tab` (`title_tabs.rs:122-125`): return `active_project_app_tab()` before
  `App(view)`.
- `scoped_tabs` (`:84-117`): `ProjectApp { project, .. } => self.project_rail_tree_on() &&
  *project == self.workspace.project` (worktree ignored on purpose; Notes gating as `App`).
- `open_title_tab` (`:188-213`): `ProjectApp { view, project }` → `open_project_app(view,
  project)`.
- `finish_close_title_tabs` (`:356-361`): also leave the view when the closing tab equals
  `active_project_app_tab()`.
- ⌘W (`shell.rs:9255-9261`): in Classic, `if let Some(tab) = active_project_app_tab() {
  close_title_tabs(&[tab]) } else { close_app_view }`.
- `select_project` (`shell.rs:4714-4723`): restore the remembered tab when it is a `Session`
  **or a `ProjectApp`** (one `matches!` arm) so coming back to a project lands on its app
  tab if that is what was open. Q3 of the draft, answered yes.
- Startup reopen (`automation_runs.rs:105`): `TitleTab::App(_) | TitleTab::ProjectApp { .. }`.
- `MenuTarget::ProjectAppTab { view, project }` (`shell.rs:377`, `:5704-5710`, `:6173-6175`,
  `pinned_tabs.rs:111`): same menu as `AppTab` (pin, close, close others/right/left/all; no
  "Send to Database", no archive — those are `tab.session()`-gated already).
- `app_title_tab` (`app_views.rs:21-132`): take the `TitleTab` (App or ProjectApp) instead of
  the bare view so the close/drag/menu/click closures carry the project. Selector stays
  `app-title-tab-{label}` — only one project's tabs are visible in a strip.
- Strip renderer (`shell.rs:8544-8553`): `ProjectApp` arm identical to `App`.
- `start_tab_drag` (`shell.rs:7990-7994`): `ProjectApp` → `None` (strip reorder only, no
  pane drop), like `App`.
- `pin_card` (`pinned_tabs.rs:156`): same as `App`. `TitleTab::session`: `None`.
- `apply_preferences` (`shell/settings.rs:347`): if `project_rail_view` changed to tree and
  an app view is open, `remember_title_tab(active_project_app_tab)`; if it changed away
  from tree, nothing (the view stays open as a surface; its tabs are hidden by
  `scoped_tabs` and kept on disk).
- Removing/archiving a project: `Workspace::restore` prunes `ProjectApp` tabs whose project
  is not in `projects` (§3.5); live removal (`shell/project_removal.rs`) adds a
  `title_order.retain` for that project's app tabs next to its session pruning.

### 3.3 Layout in tree mode — three gates
Exactly three sites switch from `app_view_open()` to `classic_app_surface()`:
1. `classic_surface_open` (`classic_surfaces.rs:8`) → footer back, `compact_surface` off,
   sidebar logic unchanged (the tree already hides it).
2. `classic_titlebar_spans_window` (`layout.rs:156`) → with the rail closed on macOS the
   strip spans the window exactly as for a session tab.
3. `classic_body`'s header choice (`layout.rs:377`) → the normal `titlebar` (strip with the
   project's tabs, pinned deck, navigation arrows) is drawn instead of
   `classic_app_surface_header`.
Everything else keeps `app_view_open()`: the right panel stays hidden under an app tab
(`shell.rs:1718-1740`, `:1799`, corner toggle disabled, `layout.rs:563`), the dock is not
drawn (`shell.rs:3594`), the terminal footer chip is absent (`terminal_dock.rs:983`), pane
hit-testing is off (`shell.rs:3935`), chat visibility trims the covered session
(`shell.rs:1357-1380`). What regresses if a gate is missed: (1) no footer, Settings-style
compact surface on macOS with the rail closed; (2) the strip jumps right by the rail width
when the rail is closed; (3) the surface header is drawn and the tab is never visible.
`navigation()` (back/forward/sidebar toggle, `shell.rs:4957`) is positioned absolutely by
`classic_body` and is unaffected. `strip_lead` / traffic-light inset are computed by
`titlebar`, so they are right by construction once gate 3 draws it.

### 3.4 Scope per app
- **Inbox**: nothing new for scoping — `open_inbox` → `set_context(projects,
  workspace.project)` already hides the other projects (`inbox.rs:610-615`). Add
  `unread_count_in(project)` for the row's dot. If the user widens the project filter inside
  the tab, it stays widened until the next project switch (today's behaviour).
- **Notes** (`notes.rs`): `scope: Option<String>` and `all_projects: bool` (not persisted),
  `set_scope(Option<String>)` called from `open_notes` with `Some(project)` in tree mode,
  `None` otherwise. The list filter (`:1330-1335`) becomes `note.matches(&query) &&
  (self.all_projects || note.in_project(self.scope.as_deref()))` — notes with no project
  show in every project (draft Q1, yes). `refresh` (`:311-318`) and `set_scope` pick the
  first *visible* note when the selection is not visible. A small "All projects" toggle in
  the list header (selector `notes-all-projects`), only drawn when `scope.is_some()`.
  `create` already uses `cwd` = the project (`:482`). `count()` / the sidebar count stay
  global.
- **Database**:
  - Core: `DataSource.projects: Vec<String>` (`db/source.rs:21-37`; `#[serde(default)]`
    gives `[]` to existing files; `[]` = visible in every project, today's behaviour,
    draft Q2 yes).
  - `DatabaseView`: `scope: Option<String>`, `projects: Vec<String>`,
    `set_scope(scope, projects)` called from `open_database` (tree mode: `Some(project)`;
    else `None`). `visible_sources()` = `projects.is_empty() || contains(scope)`; `rows()`
    (`:2400`), the tree's empty state (`render.rs:502`), `remove_source`'s fallback
    (`:2750`) and `save_source`'s selection (`:2731`) use it. `selected` must be visible: on
    `set_scope`, if it is not, take `selected_by_scope[scope]` if visible, else the first
    visible, then `ensure_consoles`. `SavedState.selected_by_scope: HashMap<String,
    String>` (new, `#[serde(default)]`), written by `save_state` (`:711-730`). No per-scope
    `active / open / expanded`: they are keyed by source already (§2.5), and copying them
    would make one source's console set differ per project, which is exactly what must not
    happen.
  - Switching scope while a query runs or a console has unsaved text: the console stays in
    `consoles` with its `task`, `conn`, `running` and editor; only `selected` moves. The
    result appears when the source is shown again; nothing is cancelled, nothing is lost.
    `console_edited`'s save path (`:1024`) is per console and keeps working off-screen.
  - Dialog (`database/dialog.rs:30-44`): `SourceDialog::new(source, editing,
    has_saved_password, projects, cx)`; a "Projects" row: an "All projects" checkbox and,
    when off, one checkbox per workspace project (names via `workspace::project_name`);
    `collect()` fills `projects`. In tree mode a *new* source defaults to the current
    project; otherwise to all.
  - `send_targets(dir, project: Option<&str>)`: filter `projects.is_empty() || contains`.
    Callers pass `FileEditor.cwd`, `Explorer.cwd`, `workspace.project`. Applied in every
    layout (assignment is data, not layout; a source assigned to other projects only is not
    offered from this project's files). The Database view itself is scoped only in tree
    mode.

### 3.5 Persistence, migration, downgrade
- **Workspace.** The on-disk `title_order / pinned_tabs / scope_focus` never contain
  `ProjectApp`. New field, ignored by older builds:
  ```rust
  #[derive(Default, Serialize, Deserialize)] #[serde(default)]
  pub(crate) struct ProjectApps {
      /// Each tab with the `title_order` slot it sat in.
      pub tabs: Vec<ProjectAppTab>,            // { view, project, slot }
      pub pinned: Vec<ProjectAppTab>,          // slot = pin index
      pub focus: HashMap<String, ProjectAppTab>, // scope key → tab
  }
  pub project_apps: ProjectApps,  // on Workspace
  ```
  `Workspace::saved(&self) -> Workspace` clones, moves every `ProjectApp` out of the three
  collections into `project_apps` (recording its index), and is what `save_state`
  serializes (`shell.rs:4312`, one-line change: `&self.workspace.saved()`). `restore` puts
  them back after `normalize_title_order` (`:537`), inserting at `min(slot, len)`, dropping
  tabs whose project is not in `projects`, then clears `project_apps`. A unit test asserts
  the saved JSON contains no `"ProjectApp"` and that save → restore is the identity for a
  mixed strip. (Not `#[serde(skip)]` on the variant: serde would make `to_string` *fail* if
  a tab ever leaked, and `save_state` unwraps it — a panic instead of a lost tab.)
- **Downgrade**: an older build reads the file, ignores `project_apps`, keeps everything
  else; the app tabs are simply absent, and lost once that build saves. Upgrading again
  starts with no app tabs — one click to reopen. No reset, no migration code.
- **Preferences** (`project_apps`), **`DataSource.projects`**, **`SavedState.selected_by_scope`**:
  all `#[serde(default)]`; older builds ignore and drop them on save (assignments would
  need to be redone after a downgrade-save; noted, accepted).
- Existing global `App` tabs in a Classic + Tree workspace stay hidden (`scoped_tabs`,
  `title_tabs.rs:87`) and reappear in Focused — unchanged.

## 4. Phases (each ships alone, builds, clippy `-D warnings`, tests green)

| # | Package | Files | Estimate |
|---|---|---|---|
| 1 | `project_apps` preference + Appearance row; `ProjectRailRow::Apps`, row rendering, selected/unread states; click = `select_project` + `open_*` (apps still open as Classic surfaces) | `preferences.rs`, `appearance.rs`, `project_rail.rs` (2 hunks), `project_rail_tree.rs`, `inbox.rs` (`unread_count_in`) | 1 day |
| 2 | `TitleTab::ProjectApp`, derived active tab, `ProjectApps` persistence (`saved`/`restore` + tests), the 3 layout gates, every strip site of §3.2, `apply_preferences` hook, project-removal prune | `workspace.rs`, `title_tabs.rs`, `app_views.rs`, `classic_surfaces.rs`, `layout.rs`, `shell.rs` (≈7 small hunks), `pinned_tabs.rs`, `automation_runs.rs`, `project_removal.rs`, `shell/settings.rs` | 3 days |
| 3 | Notes scope + "All projects"; Inbox: nothing beyond phase 1 | `notes.rs`, `app_views.rs` | ¾ day |
| 4 | `DataSource.projects`, dialog Projects row, `set_scope`, `visible_sources`, `selected_by_scope`, `send_targets(dir, project)` + 3 callers, tests | `db/source.rs`, `database.rs`, `database/dialog.rs`, `database/render.rs`, `database/send.rs`, `file_editor/selection.rs`, `explorer.rs`, `shell.rs` (1 hunk), `app_views.rs` | 2 days |

Total ≈ **6¾–7 days**. The draft's 5–6 was optimistic on phase 2 (it did not count the
persistence split, the project-removal prune, the ⌘W / `select_project` / startup arms,
the `app_title_tab` signature change, nor the test surface) and slightly on phase 4 (the
dialog checklist and three `send_targets` callers). Phase 3 is cheaper than the draft said
because the Inbox already does the work.

## 5. Tests (monocode-gpui, `#[gpui::test]`; never the real data dir — `Shell.database_dir`,
`FileEditor.database_dir`, `Explorer.database_dir` are set to a `tempfile::tempdir()` as in
`shell/database_send.rs:156-172`; `DatabaseView::new(dir)` as in `database/tests.rs:5-16`)

- Rail (in `project_rail_tree.rs` tests, fixture `tree(window, visual, &[other])`):
  `rail-app-inbox-{ix}` present only for an expanded project in Tree, absent in List;
  absent when `project_apps` lacks it; its bounds sit below the last terminal row and
  inside the project block; click → `workspace.project == other`, `active_app_view ==
  Some(Inbox)`, `visible_tabs(cx)` contains `ProjectApp{Inbox, other}`; the drag test
  (`dragging_an_expanded_project_takes_its_children_along`) extended to the Apps row.
- Strip (new test module in `project_rail_tree.rs` or `shell/project_apps.rs`): two
  projects, open Notes in each → each strip shows only its own `app-title-tab-Notes`;
  `title-tab-strip` present and `classic-app-surface-header` absent; `usage-footer` present;
  × closes to the neighbour; pin → `pinned_tabs` holds the `ProjectApp`, deck shows it;
  reorder with `reorder_title_tab`; ⌘W closes the tab (not just the view); switching
  projects and back restores the tab (`select_project` arm); toggling Tree off hides the
  tab and shows the surface header, toggling it on brings it back.
- Persistence (`workspace.rs` unit tests): `saved()` JSON has no `"ProjectApp"`;
  `restore(saved())` round-trips order, pins and focus; a saved state with `project_apps`
  for a project not in `projects` drops it; a pre-feature JSON (no field) restores unchanged
  (pattern of `restore_keeps_project_selection_and_prunes_stale_project_focus`, `:1580`).
- Regression: `classic_surface_tests.rs` (17, List mode) unchanged and green;
  `title_tabs.rs`, `pinned_tabs.rs`, `search.rs` tab tests unchanged; the Focused test
  `new_layout_app_tabs_survive_frame_changes_and_close_like_session_tabs`
  (`classic_surface_tests.rs:874`) unchanged; `layout_tests.rs` gets the "Project apps"
  row's `settings-project-apps-*` selectors (shown only with Tree).
- Notes: two notes with different `source_cwd` plus one without → scoped list shows the
  project's and the unscoped one; "All projects" shows all; `create` in scope has
  `source_cwd == scope`; selection falls to the first visible note on re-scope.
- Database (`database/tests.rs` style, temp dir): unassigned source visible in both scopes;
  a source assigned to A only in A; `selected_by_scope` survives a restart; a console with
  text and a running (never-resolving) task survives `set_scope` away and back; the dialog
  saves `projects`; `send_targets(dir, Some(B))` omits A-only sources;
  `shell/database_send.rs` tests unchanged (they pass `None`-equivalent: the test project
  is unassigned).
- Live check: `cargo build --manifest-path crates/monocode-gpui/Cargo.toml --locked -p
  monocode-gpui --profile native-dev`, run with `--dev-data --cwd PATH` on a *copy* of the
  user's data with three projects, Classic + Tree; screenshot with the rail open and closed
  (macOS strip spanning), an app tab pinned, and the Database tab in two projects.

## 6. Risks and mitigations

- **Old saved state / downgrade.** Covered by §3.5: no new on-disk `TitleTab` variant. The
  residual risk is a code path that serializes `Workspace` without `saved()`; today there is
  one (`save_state`) plus test fixtures. The round-trip test and a `debug_assert!` in
  `saved()`'s caller guard it.
- **Upstream merge exposure.** Upstream commits in the last 60 days: `shell.rs` 71,
  `layout.rs` 19, `workspace.rs` 15, `preferences.rs` 13, `project_rail.rs` 8, `inbox.rs` 8,
  `appearance.rs` 7, `title_tabs.rs` 6, `app_views.rs` 6, `classic_surfaces.rs` 5,
  `notes.rs` 2, `pinned_tabs.rs` 2. The rebase (`scripts/sync-upstream.sh --rebase`, onto
  the newest `native-v*` tag, backup branch first) replays our commits, so what hurts is our
  hunks on lines upstream edits. Mitigation: all logic in `project_rail_tree.rs` (or a new
  `shell/project_apps.rs`) and `workspace.rs`'s `ProjectApps`; upstream files get one-line
  arms and the three gates. The `TitleTab` variant makes any new upstream `match` on it
  non-exhaustive — a loud compile error, never silent. If upstream ever adds its own
  project-scoped app tabs, ours is dropped in favour of theirs (default List mode means the
  user's setup is upstream's anyway).
- **Singleton entities with several tabs.** Inbox: already re-scoped on open; the
  `hidden_projects` write is persisted (`inbox.rs:614`), so a manual widening is reset on
  the next project switch — unchanged from today. Notes: `flush` on leave and on `refresh`.
  Database: only `selected` changes per scope; consoles, tasks and connections are keyed by
  source and untouched.
- **A source assigned to no project but selected in scope A, then assigned to B only in
  the dialog** → it disappears from A's tree while selected; `save_source` falls back to the
  first visible source. Covered by a test.
- **Rail height**: +30 px per expanded project with the row on. The per-icon toggles and
  the empty-list = no row keep it optional.
- **Right panel under an app tab** stays unavailable (as today under surfaces). If the user
  wants Explorer beside the Database tab later, that is a separate change to the four
  `right_panel_*` gates and the corner toggle; not in this plan.
- **Startup**: the only place that reopens a remembered app tab at launch is the automations
  initializer (`automation_runs.rs:101-107`); if automations are not available the tab is
  in the strip but a session is active. Same as Focused today; not made worse.

## 7. Open questions (recommended answers in bold)

- Q1. A new data source created from project P's Database tab: assigned to **P** by
  default, or to all projects? **P; "All projects" is one checkbox away.**
- Q2. Should "Send to Database" honour assignments in List and Focused too, or only in
  Tree? **Everywhere — the assignment is a property of the source, and an A-only source
  offered from B's files would send into a tab B cannot see.**
- Q3. The Inbox row's unread dot: count only items whose `project_path` is this project
  (tracker items without a path never light a project's dot)? **Yes; the global rail row
  keeps the global count.**
- Q4. Selecting a project whose last active tab was an app tab lands on that tab (like a
  session) rather than on the last session? **Yes.**
