# Sleeping projects

A project you are not working on can be put to **sleep** to give back the
memory and CPU it still holds. Nothing you need to pick it up again is lost,
and **Wake project** brings it back.

## Where

- **Project menu** (right-click a project in the rail or a project space in
  the title bar): **Sleep project**, **Wake project** on a sleeping one, and
  **Sleep other projects**, which sleeps every other local project.
- **Project group menu** (right-click a rail group's header): **Sleep group**
  and **Wake group**.
- **Command palette**: **Sleep this project** and **Sleep other projects**.

## What sleeping does

| Held by an idle project | While asleep | On waking |
| --- | --- | --- |
| Dock, side-panel and title-tab shells, with any job running in them | Closed (the shell and its jobs are hung up) | Fresh shells open in the same folders, as they are shown |
| Warm, idle agent processes kept for fast follow-ups | Stopped | The next turn resumes the saved provider session (`--resume`, `thread/resume`, `session/load`) in a new process |
| Web pages | Asleep, keeping their address | Pages that were open come back |
| Loaded chat transcripts | Released | Reloaded from the database when the chat is shown |
| Background images, GPU atlases | Released | Redrawn as needed |

Kept as they are: every chat with its composer text, attachments, queued
messages and saved draft, model, effort, account and permission choices; its
tabs, splits and the active tab; file editors (with any unsaved edits); the
dock's terminal folders and page addresses. A chat with unsent text,
attachments or queued messages keeps its transcript in memory too.

A project whose chat is still answering, or waiting for an approval, stays
awake: **Sleep project** is disabled with the reason, and a group or "other
projects" sleep leaves it awake and says so. Remote projects cannot sleep.

If a terminal is running something (a dev server, a build), Sleep asks first
and lists what will stop. Idle shells close without a question.

## How it looks

- Rail rows of sleeping projects are dimmed, with a moon on the project mark
  and after the name. Title-bar project spaces carry the same moon, and their
  tooltip says **Asleep**.
- A notice tells what each sleep freed, for example
  `api: 2 agents stopped · 1 terminal closed · 3 chats unloaded.`
- Sleeping the project you are in replaces its workspace with a sleep screen:
  how long it has slept, what was freed, **Wake project**, and its five most
  recent chats. Clicking one wakes the project and opens it. The right panel's
  Terminal tab offers **Wake project** too.

## What wakes a project

**Wake project** on its sleep screen or in its menu, **Wake group**, and
whatever starts work in it: a new chat, opening or toggling a terminal, or any
turn that starts there (an automation, a Mono, the phone).

Selecting a sleeping project, in the rail (list or tree), the title bar or with
Project: Next/Previous, only shows its sleep screen. So does opening one of its
chats, files, terminals or pages from the tree, the sidebar, a tab or search:
the chat waits behind the sleep screen without loading its history, and
**Wake project** shows it. A chat clicked on the sleep screen itself wakes the
project and opens.

## Persistence

Sleeping projects are saved in the workspace state (`slept_projects`, with the
time each fell asleep) and stay asleep after a restart. Only listed local
projects are kept; archiving or deleting a project forgets it.
