# Modes: Dev and Browser

The native app has modes, switched from the **mode bar** above Settings in
the project rail (the Focused layout keeps it at the bottom of its sidebar,
above the project spaces):

- **Dev**: the workspace as it always was: projects, chats, editors, terminals.
- **Browser**: a browser in the manner of Arc, for reading, research and
  everyday browsing, with the agents one keystroke away.

The bar holds up to four modes. A button's context menu moves it left or
right, hides it (one mode always stays) and shows hidden ones; a `+` at the
bar's end does the same when a mode is hidden. Buttons can also be dragged to
another place. The order and the mode on screen are saved with the workspace
(`Workspace::mode`, `mode_bar`).

## Switching

| How | What |
| --- | --- |
| Click a mode button | Switch to it |
| ⌃1 – ⌃4 | The bar's mode at that place |
| Two-finger swipe over the rail | Next or previous mode, following the fingers |

The swipe works like Arc's spaces: the rail's body slides with the fingers,
the mode bar's pill glides along, and the Browser colour fades in as far as
the rail has moved. On lift the rail settles on the nearest mode, or the next
one when the swipe was a quick flick (`modes::Swipe::settle`). The first few
points of travel decide the axis: a vertical swipe still scrolls the lists
under it. Only trackpads (precise deltas) swipe; momentum after the lift and
mouse wheels never switch. The capture-phase listener is
`Shell::mode_swipe_catcher`.

Settings cover every mode. Picking a mode closes them.

## Browser mode

The rail becomes the browser's sidebar, laid out as Arc's (measures from
ilo's Arc parity checklist), and the page sits on an inset card:

- **Header**: the sidebar toggle, then the page on screen's **Back**,
  **Forward** and **Reload** (Stop while loading) at the sidebar's far edge.
  While they are there the page's own toolbar leaves them out
  (`BrowserPage::nav_in_sidebar`); in a Dev pane or beside the narrow rail
  the toolbar has them.
- **Address field**: the page's site, without `www.`. A click (or ⌘L) opens
  the Command Bar on it; Copy URL shows under the pointer.
- **Favourites**: icon tiles, at most 12, four to a row at most and spread
  evenly (5 as 3 + 2, 7 as 4 + 3). A double click goes back to the
  favourite's address. With none, a tab dragged under the address field
  makes one.
- **Pinned tabs**: rows above the divider, kept across quits.
- **The divider**, with **Clear** under the pointer: it sends the Today
  tabs but the one on screen to the archive. A Today tab dropped on it is pinned; a pinned
  tab or favourite dropped on it goes back to Today.
- **+ New Tab**, then **Today**: the other tabs, newest first.
- **Projects**: every project's browser pages, under the project's name and
  mark, folding away. Pages an agent opened carry a robot badge.

Rows are 36 points tall on a 39 pitch, with the favicon, the title and,
under the pointer, the close button. The selected row is a light plate over
the sidebar's colour. A loading page shows a dot where the close button
goes, and a sleeping (unloaded) page dims its icon.

### Favourites and pinned tabs

They keep the address they were pinned at (`BrowseState::home`, Arc's
Pinned URL). Away from it, the row shows the site it is on under the title
(when that is another site), its icon turns into a back arrow under the
pointer, and a favourite's tile has a dot under its icon. **Back to Pinned
URL** returns, **Replace Pinned URL with Current** keeps the new address.

⌘W, the row's − and a middle click put a pinned tab or favourite away: it
goes back to its pinned address and its page unloads; its place stays.
**Delete Tab** in its menu removes it. A link it opens goes to the top of
Today.

Drag a tab onto a tile, a pinned row or a Today row to put it there, onto
**+ New Tab** or the empty room under the list for the top of Today.

### Command Bar

⌘T, **+ New Tab** and the sidebar's menu open the Command Bar over the page;
⌘L and a click on the sidebar's address open it on the address, with the
page's address chosen (`shell/command_bar.rs`). The address is typed there:
in Browser mode the page's toolbar has no address field, only the site's
button, the password key and its tools.

It lists, as SOCode's (`browse_command::rows`): a sum's answer (Enter copies
it), the commands the words name (best five: Pin Tab, Clear Today, Undo,
Find in Page, Send to Chat, Switch to Dev Mode, …), the address typed or a
search with the profile's engine (a local address such as `localhost:5173`
is never searched), the open tabs that match (the user's and the
projects'), and the history's pages not open already. With nothing typed it
lists the tabs shown last. ↑/↓ choose (a row's address stands in the
field), Enter runs, Esc or a click outside closes. Opened for a new tab, an
empty Enter opens a blank tab, whose page lists the project's dev servers.
In Browser mode it draws in the overlay window, over the live page
(§Overlays).

- **Inline completion**: a popular domain completes what is typed at the
  end, the rest chosen (`socode_browser::omnibox::top_domain`: "git" →
  github.com): the next key replaces it, → keeps it, ⌫ takes it off. The
  domain's row comes first, then a search for what was typed.
- **The search engine's suggestions**, as SOCode's: a moment (180 ms) after
  the last key, what was typed goes to the engine's suggestion address
  (`SearchEngine::suggest_url`; Google, Bing, DuckDuckGo) on a fresh
  agent with no cookies, referrer or cache (`monocode_core::browser::suggest`),
  only for a search (not an address or a path) and only while the
  profile's suggestions are on with such an engine. Up to three come back,
  last in the list; a row's ✕ removes one for good
  (`dismiss_suggestion`). The first time they show, a note under the bar
  says what is sent, with **Search Settings…** and **Dismiss**
  (`notice_dismissed`); the ⓘ in the field shows it again.
- **Look**, as SOCode's: 764 points wide (420 on the sidebar's address),
  its field 18-point semibold in a 64-point band, 46-point rows with a
  24-point icon tile (a page's favicon, a command's glyph on a faint tile),
  the hint at the right (Switch to Tab, the tab's place, Copy) and an arrow
  tile; ⌘Z/⇧⌘Z badges on Undo/Redo. The chosen page, search or command
  row is SOCode's blue.

### Overlays

In Browser mode, menus and the Command Bar draw in a transparent,
borderless child window the size of the main window, above it
(`shell/overlay_window.rs`, as SOCode's): the live page stays under them
instead of giving way to its last picture (context.md K16). The shell opens
the child window when a menu or the bar opens in Browser mode and closes it
with them; a press on its empty room closes what is open, and a moved or
resized main window closes it. What is picked in it runs in the main
window (`Shell::in_main_window`). Elsewhere, and in tests, they draw in the
main window as before. Peek and the split drop preview have child windows
of their own (§Peek, §Split View).

Nothing else of GPUI's may draw over a page in the main window: Browser
mode has no tooltips (the mode bar's buttons say their name only in Dev
mode).

At a tab switch the page leaving stays up for 60 ms over the page coming
in, which goes under it meanwhile (`browser.rs` `SWITCH_HOLD`): Chromium
shows a page it hid with the frame it had then, or none, until it draws
again, and hidden at once the switch flashed that frame or the bare card.

### Menus' look

Browser mode's tab, folder, selection and sidebar menus use
`MenuStyle::Native`, as SOCode's and macOS's own: 24-point rows of text
without icons, the chosen row blue with white ink, submenus alike. A
tab's menu: Copy Link (⇧⌘C) · Share › · Rename… · Reset Name · Change
Icon… · Reset Icon · Mute ·
Edit Pinned Page › (pinned tabs and favourites) · Add Split View (⌃⇧=) /
Open in Split View · Duplicate (⌥D) · Move to › (Favorites, Pinned, Today,
New Folder…, each folder) · Send to Chat · Show in Dev Mode (a project's
page) · Remove Favorite / Remove Pin / Archive Tab and Archive Tabs Below.

### No toolbar

In Browser mode a page has no toolbar. Back, forward and reload are in the
sidebar's header; the address is typed in the Command Bar. Under the
pointer the end of the sidebar's address field shows, as SOCode's, Copy URL
and Site Controls (`BrowserPage::address_actions`), which opens the Site
Control Center: the toolbar's other buttons (the site's details,
passwords, history, zoom, extensions, Grab, DevTools, downloads) are there.
The address field shows at all times (`BrowserPage::address_status`) the
agent using the page (context.md K47), "Not secure" on a page without a
trusted connection (K62), "Fill as …" while a sign-in field has the focus
(K56), the picker while Grab is on, and the downloads while one is coming.
A page in Developer Mode has SOCode's developer bar over it instead.

### Site Control Center

As SOCode's (`browser/site_controls.rs`): a 236-point panel under the
address field's Site Controls button, also opened by the Command Bar's
**Site Controls** and the developer bar's first button. From the top:

- Tiles: **Capture**, **Grab** (an element for an agent, K49),
  **History**, **DevTools**.
- **Zoom**: −, the percentage (a press goes back to 100%), +.
- **Downloads** (when there are any) and **Passwords** (the site's saved
  logins, or an offer to save one): each opens its list under the button.
- **Extensions**: the extensions' action buttons; a press opens the
  extension's popup (it stays when the panel closes).
- **Settings**: each permission the site was allowed or blocked (K63),
  remembered or for this page only; a press flips it (Chromium forgets its
  own answer, so the page's next request gets the new one), × under the
  pointer resets it. Then **Developer Mode**: per site and profile, on by
  default for `localhost` and its subdomains (as SOCode's); while it is on
  the page shows the developer bar (its address in Menlo, Copy URL,
  Capture, Grab, DevTools, Reload Without Cache).
- The foot: the connection (**Secure**, **Local**, **Not Secure**), whose
  press opens the site's details (the certificate's state, K62); **More**
  (···): **Clear Cache and Refresh** (`Network.clearBrowserCache`, the
  profile's cache), **Clear Cookies and Refresh** (`Storage.clearDataForOrigin`
  with every storage type, as SOCode's), **All Site Settings…** (Settings →
  Browser). Both clears are Command Bar commands too.

The panel, the site's details, the downloads, the saved passwords and
Capture are the page's popovers (`BrowserPage::page_popovers`): in Browser
mode they draw in the child window over the live page (§Overlays), and the
page leaves them out (`popovers_elsewhere`). A press beside them or Esc
closes them; the panel also closes when the page leaves its site.

### Capture

⇧⌘2, the Command Bar's **Capture…**, the Site Control Center's first tile
and the developer bar (`browser/capture.rs`, as SOCode's). The page dims in
the child window; the element under the pointer is outlined (asked of the
page with `elementFromPoint`), and the hint at the page's foot says "Click
or drag to capture a part of this page". A click takes the element, a drag
an area. The page's own picture (CDP `Page.captureScreenshot`, as its
snapshots) is cut to it at the screen's scale, and a menu beside it offers
**Copy** (the PNG on the clipboard; the menu stays and says Copied),
**Save to Project** (`captures/capture-<host>-<time>.png` in the page's
project, else the one in front; a notice says where), **Send to Chat** (an
image attachment in the chat a grab would go to: the agent that opened the
page, else the focused chat) and **Retake**. Esc, a right click or a press
beside the page or the menu leaves; so does a page that navigates or moves.

### Split View

Up to four pages side by side or stacked, one sidebar row of segments
(`shell/browse_split.rs`, the model's `splits`). **Add Split View** (⌃⇧=)
picks the page that joins in the Command Bar; **Open in Split View** on
another tab joins it to the page on screen. The focused pane has a ring in
the Browser colour and becomes the page on screen when it takes the
keyboard. A split's menu: Separate Tab, Separate All Tabs, Add Tab…,
Convert to Vertical/Horizontal Split View, Close Pane (⌃⇧-, the focused
pane).

- **Pane headers**, as SOCode's: each pane has a 30-point header over its
  page: Back, Forward and (on the focused pane, or under the pointer)
  Reload; the site in the middle, which opens the Command Bar on that
  pane; the pane's menu and its ✕. A press focuses the pane.
- **Resizing**: the 10-point gap between two panes is their divider (a
  thumb shows under the pointer). Dragged, the two panes beside it share
  their room, neither below 220 points, snapping to even within 12 points
  (`browse_split::dragged_weights`); the model keeps the weights
  (`set_split_weights`). A double click shares the room evenly again.
- **Dragging a tab onto the page**: a sidebar tab held over the page
  targets the edge of the pane under the pointer nearest it (its outer
  quarters; top and bottom only without a side-by-side split, and only
  those in a stacked one). The half the new pane would take shows in the
  Browser colour with an outline, sliding in from the edge (180 ms, eased);
  the middle shows the pane's outline and keeps an armed edge, as SOCode's.
  The held tab shows as SOCode's 128 × 250 card at the pointer. Let go on
  an edge, it joins there. The preview draws in a child window that lets
  presses through, so the pages stay live under it
  (`browse_split::sync_browse_drop_window`).

### Peek

A pinned tab's or favourite's click to another site does not leave its
pinned site: Chromium's `OnBeforeBrowse` stops it (`monocode-cef`
`BrowserView::set_peek_host`) and the page opens in Peek, a card over the
page area (`shell/browse_peek.rs`). Its controls, round buttons beside the
card as SOCode's: Close (⌘W, Esc), Open as Tab (⌘O), which keeps the page
and its history, and Open in Split View. A press beside the card closes it.

As SOCode's, the card lives in a transparent child window of its own over
the main one: the pinned tab stays live under a veil (the sidebar's colour
at 70%), and Peek's page is a view of the child window. The card takes four
fifths of the page area's width, 12 points from the top and bottom; it
opens with a damped spring (340 ms, from 82% wide and 40 points lower) and
closes shrinking toward where the link was pressed (180 ms). Showing
another tab, or leaving Browser mode, closes Peek. Tests draw the card in
the main window instead.

### Little window

A link another app hands to Monocode, or one a routing rule sends there,
opens in the Little window, as SOCode's (Arc's Little Arc): a 1000×760
window of its own, without the sidebar (`shell/browse_little.rs`). Its
46-point header holds the traffic lights, Back, Forward and Reload, the
page's site (the window drags by it) with **Copy URL** (⇧⌘C), and **Open in
Browser** (⌘O). ⌘O moves the page, keeping it and its history, into Browser
mode as the first Today tab, as Peek's ⌘O does; the main window comes
forward. Closing the window (⌘W, its close button) drops the page and
archives nothing. A link the page opens in a new tab goes to Today.

The page is a browser pane of the shell's with no owner, as Browser mode's
own tabs (`BrowseState::little` keeps it from being pruned and is not
saved): it browses with the shared profile and Browser mode's history, and
agents' browser tools do not see it. While Chromium has the keyboard, a key
monitor of the window's own hands ⌘O, ⌘W, ⇧⌘C, ⌘R, ⌘[ and ⌘] to it.

For other apps' links to arrive, the bundle declares the `http` and
`https` schemes (`scripts/bundle-native-macos.sh`), so Monocode can be
picked as the default browser; `main.rs` passes what macOS opens to
`browser_links::open_external`.

### Link routing

Where a web link opens, as SOCode's Air Traffic Control
(`shell/browser_links.rs` `route`, the matching of `socode_browser::routing`).
Settings → Browser → **Link routing** lists the rules, in order; the first
enabled one that matches decides. A rule matches a **site** (`github.com`,
with its subdomains; a pasted address keeps its site), an address that
**contains** some text, or one address (**Is**: an address, or a bare site
without its subdomains). It sends the link to:

| Target | Opens |
| --- | --- |
| Browser mode | A tab at the top of Today, on screen |
| A project | A browser page of that project (a project gone from the workspace: the clicked window's) |
| Little window | A new Little window |
| System browser | The user's own browser |

Without a rule, a link clicked in Monocode follows K19 (a local address in
Monocode, the rest in the system browser; "Open all links in Monocode"
sends every one to Monocode), and a link from another app opens in the
Little window. ⇧-click opens what would go to the system browser in
Monocode, and anything else in the system browser. A link from another app
never goes back to the system browser (Monocode may be it): there the Little
window stands in. Links that are not web pages always go to the system.

Rules are saved with the preferences (`Preferences::browser_link_routes`).
Each row has an on/off switch, its target, Move up and Remove; the last row
makes a rule. The Command Bar's **Link Routing…** opens the page.

### The model and the archive

The sidebar's tabs live in the SOCode browser model (`crates/socode-browser`,
docs/native/browser/socode-port.md): `BrowseState::model`, with one Space.
Each model tab shows in a browser pane (`BrowseState::panes`);
`Shell::browse_reconcile` makes panes for tabs that came and closes those of
tabs that went, after every change to the model.

- **⌘W** on a Today tab, **Clear** and **Close Tabs Below** send tabs to the
  archive; **⇧⌘T** brings back the last one archived.
- Today tabs unseen for 12 hours go to the archive on their own; the page on
  screen and pages playing sound stay.
- **⌘Z / ⇧⌘Z** (outside the page, whose own editing keys win) undo and redo
  the sidebar's changes: closing, clearing, pinning, moving, folders.
- **Delete Tab** removes a tab without archiving it.

### Organising

- **Folders** live among the pinned tabs and nest, indented 16 points a
  level (`BrowseState::pinned`, a tree of `Pinned` entries). A press opens
  or closes one, a double click names it. A closed folder still shows the
  tab on screen under it. Make one with **New Folder** (the sidebar's empty
  room), **Add to Folder › New Folder** (a tab's menu: tabs all on one site
  name it for the site), or **New Folder Inside** (a folder's menu). Drop a
  tab or folder on a folder to put it at its end; on a pinned row to put it
  before that row; a folder on the divider to bring it to the top level.
  **Delete Folder** keeps its tabs where it was.
- **Names**: **Rename…** or a double click names a tab of the user's own;
  the name stands over the page's title (`BrowseState::names`) until
  **Reset Name** or an empty name.
- **Icons**: **Change Icon…** (a tab's or folder's menu) opens SOCode's
  icon picker under the row's icon: Emoji (GitHub's Gemoji catalogue,
  `assets/emoji`, searchable by name and words, with category buttons) or
  Icon (symbols, kept by their portable name such as `ion:rocket`), and
  Remove. A cell gives its icon at once; Enter gives the first emoji found.
  The icon stands in for the favicon (or the folder's icon) in the sidebar
  and on favourite tiles (`custom_icon` in the model); **Reset Icon** takes
  it off. The picker is the Command Bar's (`BarTarget::Icon`), so it draws
  in the overlay window.
- **Motion**: a row new to the sidebar (a new tab, a folder's tabs as it
  opens, a tab brought back) fades in (180 ms).
- **Rows chosen together**: ⌘-click adds a row (and the one on screen) or
  takes it out, ⇧-click chooses the rows between; a plain click lets them
  go. Their menu copies their links, pins or unpins them, adds them to a
  folder, or closes them, as ⌘W does.

### Menus

| Where | Items |
| --- | --- |
| Today tab | Copy Link · Copy Link as Markdown · Mute — Add to Favorites (⌘D) · Pin Tab · Duplicate Tab — Send to Chat — Close Tab (⌘W) · Close Tabs Below · Close Other Tabs |
| Pinned tab | Back to Pinned URL · Replace Pinned URL with Current — the copy items — Add to Favorites · Unpin Tab · Duplicate — Send to Chat — Close Tab (unloads) · Delete Tab |
| Favourite | Back to / Replace Favorite URL — the copy items — Remove from Favorites · Pin Tab · Duplicate — Send to Chat — Close Tab (unloads) · Delete Tab |
| A project's page | the copy items — Duplicate — Send to Chat · Show in Dev Mode — Close Tab |
| Today, pinned, favourite (also) | Add to Folder › (New Folder, each folder by path) · Rename… · Reset Name · Change Icon… · Reset Icon |
| Folder | Rename Folder… · Change Icon… · Reset Icon · New Folder Inside — Copy All Links · Copy All Links as Markdown · Paste URL as New Tab — Close All Tabs (unloads) · Delete Folder |
| Rows chosen together | Copy Links · Copy Links as Markdown — Pin / Unpin Tabs · Add to Folder › — Close N Tabs |
| The empty room | New Tab · Reopen Closed Tab · New Folder — Clear Today |

The window's live agents stay in the sidebar's card beneath the list, so a
working agent is always in view.

### Keys in Browser mode

| Keys | Action |
| --- | --- |
| ⌘T | Command Bar for a new tab |
| ⌘W | Today tab to the archive; pinned tab or favourite home and unloaded (a project's page closes in its project) |
| ⇧⌘T | The tab archived last comes back |
| ⌘Z / ⇧⌘Z | Undo / redo the sidebar's last change (outside the page) |
| ⌃Tab, ⇧⌘], ⌥⌘↓ / ⌃⇧Tab, ⇧⌘[, ⌥⌘↑ | Next / previous tab |
| ⌘1 – ⌘8, ⌘9 | Tab at that place, last tab |
| ⌘[ / ⌘] | Page back / forward |
| ⌘L | Command Bar on the page's address |
| ⌘D | Add to / remove from favourites |
| ⌘N | A new chat (switches to Dev) |
| Esc | Never stops an agent from Browser mode |

They are bound under the `BrowserMode` key context, which the workspace adds
while the mode is on screen. Later bindings win at the same depth, so these
take over the workspace's ⌘T, ⌘W, ⌘[ … only there. Keys typed while Chromium
has the keyboard are resolved the same way (`shell/shortcuts.rs`).

### DevTools

As SOCode's (`browser/devtools.rs`), in both modes; in Browser mode they are
reached from the sidebar address field's DevTools button, the keys and the
Command Bar.

| Command | Keys | What it does |
| --- | --- | --- |
| Developer Tools | ⌥⌘I, F12 | Open on Elements, or close |
| Inspect Elements | ⌥⌘C | Open on Elements and pick an element of the page; never closes |
| JavaScript Console | ⌥⌘J | Open on the Console; close when the Console shows |
| Network Inspector | — | Open on Network; close when Network shows |
| View Source | ⌥⌘U | The page's source (`view-source:`) in a new tab; web, file and data pages |

They open docked to the right of the page, a third of it, the page keeping
two; the divider between them resizes them, and the pane saves their size
and whether they are open (they open again with the tab, without taking the
keyboard). **Dock DevTools to Right / Left / Bottom** and **Undock DevTools
into Separate Window** (Command Bar) move them; the next DevTools open where
the last were put, for the app's run (SOCode keeps it in memory too).
Undocked they are Chromium's own window. Each pane, and each Split View
pane, has DevTools of its own.

Opened by the user they take the keyboard once loaded; closed by the user,
the page takes it back if they had it. While they have it, DevTools keep
their keys (⌘F, ⌘P, ⇧⌘P, ⌘K, ⌘R …) except the DevTools commands above
(not View Source), closing and switching tabs, and quitting
(`shell/shortcuts.rs`).

The frontend is Chromium's, docked in its own view (context.md K28), not
told it can dock: the app lays it out, so there is no dock-side menu or
close button in it (SOCode's frontend has both; its native host bridge is
not ported). The commands reach it through the frontend's own DevTools
channel, never the page's, so the agents' and the external CDP endpoint's
sessions (K38) are not touched: the panel opens with the frontend's `panel`
address parameter, and an open frontend is asked which panel it shows and
told which to show (`InspectorFrontendAPI.showPanel`,
`enterInspectElementMode`).

### Staying close to the agents

- **Send to Chat** (tab menu): the page's title and address go into the
  focused chat's composer and the chat comes forward in Dev mode.
- The element grab (⌥⌘G) works on every page and sends to the focused chat.
- Opening any chat (a live agent card, a notification, search) switches to
  Dev mode. Opening a browser page while Browser mode is on screen shows it
  there instead.
- **Show in Dev Mode** (a project page's menu) shows the page in its
  project's tab.

### The user's own tabs

They are browser panes like a project's (`workspace.browser_panes`), so the
browser's extensions, sound and mute, memory saver, passwords, permissions
and downloads work for them unchanged. They belong to no session (owner 0)
and sit in no tab of the workspace: `workspace.browse.tabs` lists them, which
keeps `Workspace::prune_browser_panes` and the restore from dropping them.
They browse with the shared default profile, keep their own history
(`monocode://browse`), and agents' browser tools do not see them.

Tab icons are fetched once per run by the address Chromium reports
(`monocode_core::browser::favicon`, `favicons.rs`) and saved with the pane;
a monogram in the site's colour stands in until one arrives.

### Media and Picture-in-Picture

As SOCode's mini player and automatic PiP.

**The media card** sits at the sidebar's foot, above the mode bar, while a
tab that has played is not on screen (in a Split View, every pane on screen
counts). One card, for the tab that started playing last; when that one
shows, the next tab with media takes it. Its 33-point row holds **Back to
Tab** (the tab's icon), **Previous Track**, **Play/Pause**, **Next Track**
and the tab's **Mute**. Previous and Next are dim unless the page handles
them (Media Session). Under the pointer a header rises over the list
(200 ms) with the title (the Media Session's, else the page's; a click shows
the tab), **Open Picture in Picture** (videos only) and **Close Mini
Player**, which hides the card while the media plays on, until the tab
starts playing again. The header falls two seconds after the pointer
leaves. Drawing: `shell/browse_media.rs`.

**Automatic Picture-in-Picture**: a page that stops being shown (another
tab, another mode) while its video plays with sound opens Chromium's own
PiP window; showing the page again closes it. A muted video, audio, a
paused video and a page under an overlay (the Command Bar) stay put. The
PiP window's **Back to Tab** shows the tab: Chromium's Back to Tab closes
the window and the video plays on, its Close pauses, so a PiP that ends on
its own while the page is out of sight and still playing was Back to Tab
(`browser_playback.rs`, `MediaReturn`). **Settings → Browser → Picture in
Picture when you leave a video tab** turns it off (`browser_auto_pip`).

The page tells what it plays: a script in the top document's main world
(`monocode_core::browser::playback`) picks its media element, wraps the
Media Session's `setActionHandler` to know Previous/Next, and reports
through a binding it takes off the global in every frame. Commands and
PiP go back as fixed `Runtime.evaluate` expressions with a user gesture.

Not yet: the tab volume slider (the card mutes instead), the reference's
own PiP window controls (its patched Chromium), media in iframes and
shadow roots, Document PiP.

## Colour

Browser mode washes the sidebar and the frame around its page with a colour,
as Arc's spaces do: Violet by default, or Blue, Teal, Green, Amber, Rose, or
Graphite (none), from the Browser button's menu. The page's card stays opaque.
The wash and the selected tab's fill follow the theme (lighter and weaker in
light themes). In Dev mode nothing changes.

## Development

`MONOCODE_MODES_DEV=<file>` runs commands a script writes to the file
(`mode dev|browser`, `tab <url>`, `favorite`, `pin`, `folder <name>`, `name <text>`, `tint <name>`, `hold <pos>`,
`release`, `compact on|off`, `group <url>`, `shot <png>`) and logs to
`<file>.log`. `shot` draws the window off screen and needs a
`--features selftest` build; Chromium's pages are not in the picture.
