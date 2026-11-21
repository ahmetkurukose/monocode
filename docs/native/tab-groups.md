# Tab groups

The native workspace has one tab layer. A project's checkout (its *scope*:
project + worktree) shows one split layout; every split region is a **tab
group** with its own strip. There is no title-bar tab strip, no pinned deck
and no second strip inside a pane: a file, diff or commit is a tab like a
chat, a terminal or a browser page.

```
scope (project + worktree)
└── one title tab: a split tree (PaneNode)
    └── leaf = the visible tab of one group
        group = ordered tabs: chat · file · diff · commit · terminal · browser · Mono · view
```

## Model (`workspace.rs`)

The split tree stays the source of truth for what is on screen: **every leaf
of a `PaneNode` is the visible tab of a group**; the group adds the hidden
tabs stacked under it.

- `Workspace::groups: Vec<TabGroup>`, `TabGroup { tabs, pinned, preview, mru }`
  (`mru` is not saved). Tabs are pane ids: sessions are positive; file,
  terminal, browser, Mono and view panes are negative (`next_pane_id`).
- A group has no id. It is found through any member (`group_of`,
  `group_tabs`, `visible_of`); a leaf without a group is a group of one
  (`normalize_groups`).
- Switching a tab replaces the leaf (`show` → `PaneNode::replace_leaf`), so
  surfaces, drop hints, pane motion and focus keep reading "leaf = pane on
  screen".
- `take_out` understands groups: a hidden tab leaves its group; a visible one
  is replaced by the group's most recently used tab; only a group's last tab
  removes the leaf (the split collapses with pane motion). `close_pane`,
  `close_in_group`, `place`, `split` and `add_to_group` build on it.
- `tab_of` resolves hidden tabs through their group, and `open_sessions`
  includes them, so "is it open" checks keep their meaning.
- **One title tab per scope.** `tabs`/`layouts` stay the per-scope container,
  but whatever opens in a scope that already has one joins a group there.
  `merge_scope_tabs` folds extra title tabs of a scope into its focused group
  (migration and safety net).
- **Files are panes.** Every open file, diff or commit is its own file pane
  (`review_owners: pane → owner session`, a `ReviewState` with one
  `ReviewTab`). Terminal and browser panes record an owner too. The owner is
  only the chat that opened it: it gives the pane its project.
- **Views are panes.** Inbox, Notes, Automations, Database, API and Search are
  `view_panes` (scoped by `view_scopes`); Monos are `mono_panes`. Each is a
  singleton per scope (`shell/view_panes.rs`).

## Opening (`shell/tab_router.rs`)

`open_in_workspace` (sessions, panes) and `open_review_tab` (files, diffs,
commits) decide dedup and placement:

1. **Dedup per scope.** Something already a tab of the scope (a session, a
   file pane whose tab is `ReviewTab::same_tab`, a Mono, a view) is shown and
   flashed. A file pane closed with its tab but holding unsaved edits counts
   and comes back.
2. **Target group** (`spot`). An explicit group (a strip's `+`, a drop,
   Reopen) wins. Otherwise the focused group, except that files never cover a
   chat or terminal: they go to the group showing an editor, else one holding
   one, else a new group split to the right of the focused one. The scope's
   first tab starts its title tab.
3. **Preview.** A single click in the explorer, Changes, search results or
   history opens a *preview* (`OpenIn::PREVIEW`, italic title). The group's
   next preview replaces it in place unless it has unsaved edits. Opening it
   for real, double-clicking its tab, editing it or its menu's Keep Open makes
   it permanent (`keep_preview`).
4. A new tab is inserted right after the group's visible tab; pinned tabs
   stay first.

## Strip (`shell/group_strip.rs`)

Each group draws one strip at its top in the title-tab plate style (`mark_tile`
session marks with their working animation, `tab_label`).

- Tabs: icon + title + close; a dirty dot for unsaved files; italic for a
  preview. Pinned tabs are icon-only plates at the start.
- Trailing `+` (New Session, New Terminal, New Browser, Open File…) opens in
  this group; `⋯` (Split Right, Split Down, Equalize Sizes, Close Group).
  Empty strip space drags the window; a double click there starts a session
  in this group.
- Tab menu: Close, Close Others, Close to the Right, Close All (all scoped to
  the group, pins kept), Pin/Unpin, Keep Open (previews), Split Right/Down,
  Send to Database (files), and for chats Rename, Copy Session ID, Archive,
  Delete.
- Strips along the window's top edge are the title bar (`strip_insets`): the
  navigation (traffic lights, sidebar toggle, visit arrows) and the corner
  controls float over the room the top-left and top-right strips leave. The
  separate title row comes back only where no strip can stand in for it
  (`strips_are_titlebar`): Settings, sleeping or empty projects, and a
  terminal dock beside or above the panes. App views, Search and Mono tabs
  are tabs like any other and keep the strips as the title bar: a focused
  Mono's overview controls (Overview, Sub-agents, Documents, Habits,
  Changes, Activity) take the corner in place of the project's panel tabs,
  over the header of the details panel they switch, and the corner's panel
  toggle shows and hides those details.
- The focused group's visible tab is highlighted; other groups' bodies are
  slightly dimmed.

## Drag and drop

- Within its strip a tab reorders with the tab shift motion (`tab_motion`).
- Over another strip: it joins there at the pointer.
- Over a pane body: the edge drop hints split; the centre moves the tab into
  that group. A group's only tab dropped on its own group does nothing.
- Sidebar sessions and dock terminals use the same targets. Explorer files
  join a strip, or the edges and middle of a non-chat pane; over a chat they
  stay attachments.
- A group left empty disappears and its neighbour grows.

## Closing and keys

| Key | Action |
| --- | --- |
| ⌘W | Close the focused tab (its group's MRU tab takes over; the last tab closes the group) |
| ⌘⇧T | Reopen the scope's last closed tab, in its old group when that is still open (30 per scope, this run only) |
| ⌘1…⌘8, ⌘9 | Tab n / last tab of the focused group |
| ⌘⇧[ ⌘⇧], ctrl-tab | Previous / next tab in the focused group |
| ⌘D / ⌘⇧D | New session split right / below |
| ⌘⌥ arrows | Focus the neighbouring group |

- Closing a chat's tab closes that tab only. It asks whether to archive the
  chat (Settings › General, "Closing a session tab"), never stops or deletes
  it, and never closes the files, terminals or pages it opened. Archiving a
  chat closes only its own tab too.
- A dirty file asks Save / Discard / Cancel; a terminal with a running process
  asks before it closes.
- **Deleting a chat** hands its panes to another chat of the project
  (`shell/session_handover.rs`): an open one in the same group, else in the
  scope's title tab, else open elsewhere, else any chat of the project
  (unarchived first). Only when the project has no other chat do the panes
  close, after one Save / Discard / Cancel for unsaved files and the running
  terminal question.

## Persistence and migration

`groups` (minus `mru`), `view_panes`/`view_scopes` and the pane maps are saved
with the workspace; the Reopen stack is not. Loading an older workspace
(`migrate_tab_groups`, `migrate_view_tabs`): each file list attached beside
a chat becomes one file pane per file stacked after that chat; extra session
title tabs of a scope merge into its focused group; title-strip app, Search
and Mono tabs become view panes in the scope they were focused in. The old
"File tabs" preference is still read and written for older builds but has no
effect.
