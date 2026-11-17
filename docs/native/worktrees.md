# Worktrees (GPUI) — Orca-style model

Status: implemented (core, every create caller, Create/Delete dialogs, Settings, rail
tree rows). Scope: the native GPUI app only (`crates/monocode-core`,
`crates/monocode-gpui`). Tauri/React and `host/` are untouched. Remote (SSH) projects keep
the old `git::create_project_worktree` → `git::remote` path from a reduced Create dialog
(branch, start from, existing branch); everything below is local-only. User-facing summary:
[README › Worktrees](README.md#worktrees).

## 0. Decisions (from the user)

| Topic | Decision |
| --- | --- |
| Location | Central root, Orca-style: `~/monocode/workspaces/<repo>/<slug>`, changeable in Settings |
| Default | A session uses a worktree only when the user explicitly asks (composer "New worktree", dialog) |
| Scope | GPUI only |
| Existing `mc-native-*` worktrees | Stay where they are, keep working; shown as **legacy** |
| Config | `monocode.yaml` at repo root, same schema as Orca's `orca.yaml`; if absent, `orca.yaml` is read. Plus `.worktreeinclude`. |

## 1. Model

A worktree is a first-class object owned by a project (the main checkout), not a
session setting. Git is the source of truth for structure (`git worktree list
--porcelain -z`); Monocode stores only overlay metadata.

- **id / key**: `files::path_key(canonical path)`.
- **WorktreeMeta** (native settings, key `worktree:<path_key>`, JSON):
  `path, repo (main checkout path), display_name, branch_at_create, base_ref,
  start_kind (base|branch|commit|remote), created_at, created_by ("dialog"|"composer"|
  "automation"|"api"|"switcher"), branch_user_chosen, preserve_branch_on_delete,
  pinned, comment, parent (path or None)`.
- **Ownership** (never inferred from path shape alone):
  - `Managed` — has a WorktreeMeta.
  - `Legacy` — no meta, path under the old sibling root `<main>-worktrees/`.
  - `External` — anything else (e.g. `.claude/worktrees/*`, user's own `git worktree add`).
  - `Main` — the primary checkout.
- **Repo prefs** (native settings key `worktree-repo:<path_key of main>`): `default_base`
  (e.g. `origin/main`), `setup_approved_hash` (sha256 of trimmed setup script),
  `show_external` (bool, default false).
- **Global prefs** (native settings key `worktree-prefs`): `root` (default
  `~/monocode/workspaces`, or `$MONOCODE_WORKTREE_ROOT` when that environment variable is set
  and non-empty), `branch_prefix` (`none` default | `git-username` | `custom:<p>`).

## 2. Core API (`monocode_core::worktree`, new module dir)

```
layout::default_root() -> PathBuf                     // $MONOCODE_WORKTREE_ROOT or ~/monocode/workspaces
layout::repo_dir(root, main) -> PathBuf               // root/<basename(main) minus .git>
naming::slug(name) -> Result<String>                  // see §3
naming::branch_for(slug, prefix, cwd) -> String
base::detect_default(cwd) -> Option<String>           // origin/HEAD → origin/main → origin/master → main → master
base::resolve(cwd, explicit, repo_default) -> Resolved{ reference, qualified, remote: Option<(remote, branch)> }
create::create(req: CreateRequest, progress: &dyn Fn(Phase)) -> Result<Created, String>
content::materialize(main, worktree, config) -> Report   // shared dirs + .worktreeinclude
config::load(main) -> Config                           // monocode.yaml → orca.yaml
setup::script(config) / setup::hash(script) / setup::env(main, worktree, name)
catalog::list(cwd, store) -> Vec<Entry>                // git list ⨝ meta, ownership, locked, prunable
catalog::reconcile(cwd, store)                         // drop meta whose path is gone & not registered
remove::preflight(cwd, path) -> Preflight              // dirty, at-risk ignored files, unpushed, locked
remove::remove(cwd, path, opts) -> Removed{ preserved_branch: Option<String>, warnings }
remove::force_delete_branch(cwd, branch)
meta::{load, save, delete, all}(store, ...)
```

`Phase` = `Resolving | Fetching | CheckingOut{percent:Option<u8>} | CopyingFiles | Done`.

## 3. Naming rules

- Slug: replace runs of chars not in `[\p{L}\p{N}._-]` with `-`; collapse `--`; `..+` → `.`;
  strip leading/trailing `.`/`-`; max 64 chars (char-boundary safe); empty/`.`/`..` → error.
- Branch: `<prefix>/<slug>` or `<slug>` (prefix `none`). `git-username` = `git config user.name`
  slugified, dropped silently if invalid. A leaf ending `.lock` → `-lock`. Validate with
  `git check-ref-format --branch`.
- Explicit branch override (Advanced field) wins and is used as-is (validated).
- Collisions: try `slug`, `slug-2` … `slug-100`, rejecting a candidate when the local branch
  exists, `origin/<branch>` exists, the path exists, or another in-process create holds the
  path claim (a global `Mutex<HashSet<PathBuf>>`, released on drop). Explicit branch override
  never gets a suffix (error instead), but its folder may be suffixed.
- Composer "New worktree" with no name: slug from the first ~6 words of the first prompt
  (fallback `worktree`). The existing provider-naming step may later rename the **branch**
  only (folder stays), applying the configured prefix instead of a hardcoded `mc/`; it is
  skipped when `branch_user_chosen`.

## 4. Creation pipeline

1. Resolve base: explicit start-from → repo `default_base` → `detect_default` → `HEAD`.
   Qualify: name with `/` tries `refs/remotes/X` then `refs/heads/X`; else `refs/heads/X`,
   then any commit-ish (SHA) via `rev-parse --verify --end-of-options X^{commit}`.
2. If the base is a remote-tracking ref: `git fetch --no-tags <remote> <branch>` with a 60 s
   timeout. Failure → continue with the existing local tracking ref and add a warning.
   (Phase `Fetching`.)
3. Name + collision loop (§3), path claim held until the end.
4. `mkdir -p <repo_dir>`; `git -c checkout.workers=4 worktree add --progress --no-track -b <branch> -- <path> <sha>`
   (existing branch: `worktree add -- <path> <branch>`; remote branch: `worktree add --track -b <branch> -- <path> <remote>/<branch>`).
   Parse `Updating files: NN%` from stderr into `CheckingOut{percent}`.
5. `git config --local branch.<b>.base <base ref>`; if `push.autoSetupRemote` unset at every
   scope → `git config --local push.autoSetupRemote true`.
6. Save WorktreeMeta (before content paths, so a crash still leaves an owned row).
7. Content paths (`CopyingFiles`): §5. Failures are warnings, never abort.
8. Return `Created{ path, branch, base, warnings, setup: Option<SetupPlan> }`.
On failure after step 4: `git worktree remove --force` + `git branch -D` only for a branch this
call just created, delete meta; never touch pre-existing branches.

## 5. Content paths and setup

- Config (`monocode.yaml`, else `orca.yaml`, read from the **main** checkout; max 256 KiB;
  unknown keys ignored; parse error → warning, empty config):
  ```yaml
  worktree:
    sharedDirectories: [node_modules, .cache]
  scripts:
    setup: |
      npm install
  ```
- `sharedDirectories`: relative, no `..`/absolute/`.git`; must exist as a dir in main and be
  ignored (`git check-ignore -q`). Created as symlink `worktree/<p> -> main/<p>`; skipped if the
  destination exists.
- `.worktreeinclude` (main root, ≤256 KiB, ≤1000 lines): literal relative paths; `#` comments,
  blanks; lines with `!`, `*`, `?`, `..`, absolute, `.git` skipped with warning. Must exist in main
  and be ignored. Copied (never overwrite); macOS tries APFS clone `cp -c -R`, else plain copy.
  Budget 2 GiB / 50 000 entries, overflow skipped with warning. Paths already shared are skipped.
- Setup script: `scripts.setup` from the **new worktree's** config (falls back to main). Trust:
  sha256 of the trimmed script vs repo `setup_approved_hash`. The create dialog shows the script
  when it is new/changed; choosing "Run setup" approves the hash. Runner written to
  `$(git rev-parse --git-path monocode/setup-runner.sh)` (`#!/usr/bin/env bash`, `set -e`, chmod 755),
  run in a dock terminal titled "Setup" in the new worktree, with env `MONOCODE_ROOT_PATH`,
  `MONOCODE_WORKTREE_PATH`, `MONOCODE_WORKSPACE_NAME` plus Orca aliases `ORCA_ROOT_PATH`,
  `ORCA_WORKTREE_PATH`, `ORCA_WORKSPACE_NAME`.

## 6. Removal

- Preflight: refuse main, unregistered, locked, detached-without-branch (keep today's message),
  and a worktree containing another registered worktree. Report: dirty entries
  (`status --porcelain --untracked-files=all`, ignoring our shared symlinks), at-risk ignored
  files (`ls-files -z --others --ignored --exclude-standard --directory --no-empty-directory`,
  minus regenerable dirs `node_modules dist build target .venv venv __pycache__ .next .turbo .cache`
  and shared symlinks; bounded 5000 paths; `.env*` sorted first), unpushed commit count.
- Remove: `provider::discard_worktree`, unlink our shared symlinks, `git worktree remove [--force]`
  (`--force` only when the user confirmed dirty/at-risk), then delete the branch with
  `git branch -d` unless `preserve_branch_on_delete` or the branch is checked out elsewhere;
  if `-d` refuses → `preserved_branch = Some(b)` (UI offers "Delete anyway" = `-D`).
  Then `git worktree prune`, delete meta. Legacy/external worktrees: same flow but never delete
  the branch (keep today's behaviour).

## 7. GPUI surfaces

1. **Create Worktree dialog** (new): Name (placeholder generated), Start from (base picker:
   default base, local branches, remote branches, SHA text), "Set as default" for the base,
   Advanced: branch name override, "Use existing branch" when the typed branch exists,
   Setup: Run / Skip (shown only when a script exists; shows the script if not yet approved).
   Entry points: worktree switcher "New worktree…", rail project context menu "New worktree…",
   Settings → Worktrees "Create". Submit closes the dialog; creation runs in the background.
2. **Pending creation**: a row under the project in the rail tree and in the switcher with the
   phase text / percent; failure → toast with Retry. Success → the worktree becomes the
   project's focus (`select_worktree`) and the setup terminal opens when chosen.
3. **Rail tree**: project → its worktrees (managed + legacy, non-main; external only if the
   repo's `show_external`) as expandable rows listing the sessions whose `worktree_cwd` is that
   path; the project's own sessions stay directly under the project. Row: display name,
   branch, session count; context menu: Open, New session here, Copy path, Reveal, Delete…
   (`shell/project_rail_worktrees.rs`). The list is read with `catalog::list_in` in the
   background only for an open project whose row is drawn, cached per project, and marked
   stale after a create, a delete, a switcher metadata change or a Settings worktree pref
   change. Open worktrees persist in `Workspace.project_rail_worktrees_expanded`.
4. **Delete Worktree dialog**: preflight summary (dirty count, at-risk files sample, unpushed),
   "Also delete its sessions", Delete / Force delete; preserved-branch toast with "Delete branch".
5. **Composer "New worktree"**: unchanged UX, now uses `worktree::create` with
   `created_by=composer`, default base from §4, name from the prompt.
6. **Settings → Worktrees**: Location (root), Branch prefix, per-project default base and
   "Show other worktrees"; list gains ownership label and uses the new Delete dialog.
7. All other create callers (`app_access`, monos host `worktrees.create`, app commands,
   automations "Fresh worktree", quick composer, mobile host) go through `worktree::create`.

## 8. Test isolation

`layout::default_root()` honours `MONOCODE_WORKTREE_ROOT` (set and non-empty) before the home
directory. Tests never write to the real `~/monocode`: `layout::isolate_for_tests()` sets the
variable once per process to `<temp>/monocode-test-<pid>/monocode/workspaces` unless the
environment already sets it. monocode-core calls it from `default_root()` under `cfg(test)` and
from the worktree fixtures; the GPUI test `Shell` constructor calls it before anything else.
Tests that check paths still set an explicit `worktree-prefs.root` in their temp dir.
