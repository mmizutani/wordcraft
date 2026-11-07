# Control protocol

Start the app with a control port: `wordcraft --control 7981` (or `WORDCRAFT_CONTROL_PORT=7981`).
A missing or invalid port after `--control` (not 1–65535) prints an error and exits with status 2.
It listens on `127.0.0.1` only. Send one JSON object per line, with the window's key (see
[Keys](#keys)); each gets one reply line:

```json
{"id": 1, "key": "…", "method": "engine.execute", "params": {"command": "text.insert", "params": {"text": "Hello"}}}
{"id": 1, "ok": true, "result": {"anchor": {...}, "focus": {...}}}
```

Errors come back as `{"id": …, "ok": false, "error": "message"}`. A failed command leaves the
document unchanged. A line that isn't valid JSON gets one error reply, then the server closes the
connection (so an HTTP request sent to the port can't run a command); reconnect to continue.

## Keys

Any local program (and possibly a web page) can reach a loopback port, so every request needs the
window's key. Once the port is bound, the app writes a random key (256 bits, as hex) to
`<settings>/control-key.<instance>` and removes the file when the window closes; a new start
writes a new key. `WORDCRAFT_PROFILE_DIR` overrides the whole profile folder for isolated runs,
including recovery copies. See [local recovery](recovery.md). `<settings>` is the folder that holds the preferences: on Linux
`$XDG_CONFIG_HOME/wordcraft` (by default `~/.config/wordcraft`), on macOS
`~/Library/Application Support/WordCraft`, on Windows `%APPDATA%\WordCraft`. `<instance>` is the
Flatpak instance id inside Flatpak, and `local-<port>` otherwise. On Unix the file has mode 0600.

```sh
cat ~/.config/wordcraft/control-key.local-7981   # Linux, outside Flatpak, port 7981
```

- Send the key as top-level `"key"` with every request, or once per connection with
  `{"method": "auth", "key": "…"}` (reply `{"ok": true, "result": {}}`); later requests on that
  connection may leave `key` out. Only `auth` reads `params.key`; for other methods `params.key`
  is an ordinary parameter (`ui.key {"key": "Enter"}` presses Enter).
- The key is checked on every request. A missing or wrong key gets
  `{"ok": false, "error": "unauthorized"}`, and the app closes the connection.
- A line over 1 MiB gets `{"ok": false, "error": "line too long"}`, and the app closes the
  connection.
- A connection that sends no authorised request within 30 s of being accepted is closed. That is
  one limit: blank lines and bad JSON do not extend it. After an authorised request the
  connection may stay idle.

The key of an invite to the document the window shares gets its holder a copy of that document
of their own instead of the window: see [Shared documents](#shared-documents).

`wordcraft-cli mcp --connect` takes the key from `WORDCRAFT_CONTROL_KEY`, or else from the key
file for the port it connects to, and reads it again on every new connection (the app may have
restarted). It sends the key only to the app's own address, `127.0.0.1` (`localhost` connects
there too, never to `::1`): another loopback address such as `::1` or `127.0.0.2` is not where the
app listens, so whoever listens there gets no key (#3699). For
any other host it sends no key, says so, and the app refuses its requests: to drive an app on
another machine, forward its port to `127.0.0.1` (for example `ssh -L 7981:127.0.0.1:7981 host`)
and set `WORDCRAFT_CONTROL_KEY` to that app's key.

## One request from the command line

`wordcraft-cli call <method> [<params> | -] [--connect HOST:PORT]` sends one request and prints
its result as JSON, for scripts and for agents that work through a shell. `params` is a JSON
object (`-` reads it from standard input, which saves quoting); without it the request has `{}`.
The address is `--connect`'s, else `WORDCRAFT_CONTROL_ADDR`'s, and the key is found as for
`wordcraft-cli mcp --connect`. It refuses any address but `127.0.0.1` or `localhost` before it
connects. An error, from the app or on the way, goes to standard error on one line (with the
app's control characters shown escaped) and the exit status is 1.

```sh
wordcraft-cli call document.text --connect 127.0.0.1:7981
echo '{"text": "Hello"}' | wordcraft-cli call text.insert - --connect 127.0.0.1:7981
```

## Methods

| Method | Params | Result |
|---|---|---|
| `engine.execute` | `command`, `params`, `doc?` | the command's result (any id from `engine.commands`, including UI commands `ui.tab`, `ui.dialog` (`{"name": dialog, "part"?: table style part}`; `part` opens New / Modify Table Style (`newTableStyle` / `modifyTableStyle`) on that part: `wholeTable`, `headerRow`, `firstColumn`, `bandedRows`, `nwCell`… as `table.modifyStyle` names them), `ui.backstage`, `ui.zotero.*` (see `docs/zotero.md`), `ui.language` (`{"value": "auto"\|"en"\|"zh-hans"\|"zh-hant"\|"ja"\|"pt-br"\|"es"\|"uk"\|"sr"\|"sr-latn"\|"et"\|"tr"\|"ru"\|"pl"\|"de"\|"it"\|"cs"\|"hu"\|"fr"}`), `ui.theme` (`{"value": "system"\|"light"\|"dark"}`; `system` follows the OS appearance, light when it reports none), `ui.interfaceSize` (`{"value": "auto"}` or `{"percent": 50–300}`, clamped; no value reads it; `auto` keeps the system display scale, a percentage is absolute, so 100 is one screen pixel per point (#822); the result's `percent` is the size drawn at and `auto` whether it follows the system; document zoom stays `view.zoom`), `ui.animations` (`{"value": "system"\|"on"\|"off"}`; no value reads it; `system` follows the system's animation setting, which the desktop app reads on Windows, macOS, Linux and the BSDs and the web app from `prefers-reduced-motion`, and means on where the system doesn't say; the result's `enabled` is what the setting comes to and `system` what the system reports, `null` when it doesn't say), `ui.fileBrowser` (desktop: `{"purpose": "document"\|"picture"\|"recipients"\|"save"}` opens WordCraft's own file browser, for when the system file dialog doesn't show up, #576), `ui.saveAsTemplate` (File › Save As › Save as Word Template: the Save dialog for a `.dotx`, starting in the user templates folder; the result's `dir` is that folder, `null` on the web, #2852), `ui.caretAnimation` (`{"value": true\|false}`; no value reads it; off, the caret jumps and blinks on and off instead of gliding and fading), `ui.startScreen` (`{"value": true\|false}`; no value reads it; on, the default, the desktop and web apps start on File › Home when launched with nothing to open, #766), `ui.displayServer` (`{"value": "auto"\|"wayland"\|"x11"}`; no value reads it; File › Options › Advanced › Linux display server, which the desktop app reads on Linux and the BSDs at its next launch; `WORDCRAFT_WAYLAND` overrides it, #2184), `ui.alignmentGuides` (`{"value": true\|false}`; no value reads it; on, the default, moving a picture or shape snaps it to the page, the margins and other objects, with guide lines while dragging; Alt moves freely), `ui.quickAccessToolbar` (`{"value": true\|false}`; no value toggles it; false takes the Quick Access Toolbar out of the title bar, as its menu's Hide Quick Access Toolbar does, and true brings it back, #1945), `ui.statusCount` (`{"value": "words"\|"characters"\|"charactersNoSpaces"}`; no value reads it; what the status bar's count shows — words (the default), characters with spaces, or characters without spaces — kept between runs, #2797), `ui.focusRibbon` (`{"value": true\|false}`; no value toggles it; in Focus, true drops the ribbon down over the page, as the bar's ⋯ does, and false puts it away, as a click on the page does; outside Focus it stays put away, #1678), `ui.contextMenu` (opens the page's context menu below the caret, as Shift+F10 does), `ui.paneDock` (`{"pane": "navigation"\|"clipboard"\|"styles"\|"styleInspector"\|"accessibility"\|"altText"\|"formatShape"\|"restrictEditing"\|"comments"\|"smartArtText", "dock"?: "left"\|"right"\|"float", "before"?: pane\|null}`; no `dock` reads it; docks or floats a task pane as its menu does, kept between runs; Styles is `view.stylesPaneDock`, #1756; `before` puts the pane directly before that pane in the order, outside it on their edge, as a drop over a docked pane's outer third does, and `null` after every pane; the order is kept between runs)…) |
| `engine.commands` | — | every command: id, label, location, shortcut, params, mutates, `scope` and `positions` (as MCP's `list_commands` gives them, see [Command scopes](mcp.md#command-scopes)), enabled |
| `document.inspect` | `text?`, `doc?` | blocks (`id`, text, style, runs, lists, tables), parts, sections, selection, pages; `version`, for `document.changes` |
| `ui.inspect` | — | UI state, view, dialog, window size, page rects on screen, caret, comment balloons (`balloons`: id + screen rect; `balloon`: the selected one), perf; `document`: the active document's id; `documents`: the open documents (`id`, `title`, `path`, `dirty`, `active`) |
| `ui.click` | `x`, `y`, `button?`, `count?`, `shift?`, `cmd?`, `alt?` | real pointer input at window coordinates |
| `ui.clickText` | `page` (0-based), `x`, `y` (points from the page's top-left), `count?` | click inside a page |
| `ui.move` / `ui.drag` | `x`,`y` / `x`,`y`,`toX`,`toY`,`steps?` | pointer move / drag |
| `ui.press` / `ui.release` | `x`,`y`, modifiers | primary button down / up there (with `ui.move` between: a drag you can screenshot halfway) |
| `ui.wheel` | `x`, `y`, `dy`, `dx?`, `unit?` (`point`/`line`/`page`), `phase?` (`start`/`move`/`end`), modifiers | mouse-wheel or trackpad scroll at window coordinates (`dy` < 0 scrolls down) |
| `ui.key` | `key` (`B`, `Enter`, `Left`…), `shift?`, `alt?`, `cmd?` | key press through egui (shortcuts apply) |
| `ui.text` | `text` | typed text through egui |
| `ui.screenshot` | `path?` | PNG of the window: written to `path`, or without one returned as base64 `png` in the reply |
| `ui.render` | `path?`, `page?` (1-based), `scale?`, `doc?` | PNG of a page, rendered by the engine: written to `path`, or without one returned as base64 `png` in the reply (never a fixed name in the shared temporary folder) |
| `ui.parity` | — | feature-catalog parity |
| `ui.resize` | `width`, `height` | asks for that window content size in points; sizes that aren't positive and finite are refused, the rest are clamped to the app's minimum (760 × 480) and to the largest surface the GPU can draw (at most 16384; that limit wins over the minimum), and the reply's `width` and `height` are the size asked for |
| `ui.focus` / `app.quit` | | window control |
| `room.share`, `room.invite`, `room.revoke`, `room.role`, `room.participants`, `room.stop` | see [Shared documents](#shared-documents) | sharing the document the window shows with agents and people who join with a key |

Any other method name is treated as a command id: `{"method": "format.bold"}` works.

**Task pane docking.** `ui.paneDock` and `view.stylesPaneDock` use **physical** `left` / `right`
(the window's left and right edges), matching Dock Left / Dock Right in the pane menu. That stays
true in a right-to-left interface language: `{"dock":"left"}` always puts the pane on the physical
left and the command reports `"left"`. A pane that has never been docked keeps its default
start-/end-edge role when mirrored: start-default panes (e.g. Navigation) open on the physical
right in Arabic, end-default panes (e.g. Comments) on the physical left. Saved docks and explicit
requests are never flipped.

Language packs (see [MCP](mcp.md#language-packs)) are commands like any other: `proof.packs`,
`proof.installPack {"locale": "nl"}` and `proof.removePack {"locale": "nl"}`. Through the control
channel they act on the running app's packs, and install only while its downloading setting is on;
they never turn it on, and neither does a `ui.click` or `ui.key` on the app's Install, Add a
language… or Refresh buttons. The assistant's consent is kept the same way: `ui.click` and
`ui.key` never answer its permission or sign-in questions, never turn on File › Options ›
Assistant › Allow what agents ask when no one can answer (`assist.allowUnattended` turns it off
but not on), and a prompt they send from the Assistant pane is a program's, not the person's:
its agent's requests are answered per that setting. An install or a refresh runs in the background
(`"started": true`); `proof.packs` reports its progress, then the result.
`ui.dialog {"name": "languageData"}` opens About › Language data.

### Open documents

Several documents can be open at once. Each has an id, a whole number that stays the same while
the document is open and is never reused by the running app. On the desktop each open document
has a native window of its own; the web app shows them one at a time in its one window. One
document is the active one: on the desktop, the document of the window focused last (the active
window); in the browser, the one the window shows. Commands work on it unless told otherwise.

- `file.new` opens another document in a new window, focused, and makes it active. `file.open`
  does the same with a file; it replaces a blank document nobody has used yet in that document's
  window, and a file that is already open becomes active again and its window comes to the front
  (`"alreadyOpen": true`). Its result carries the document's `id`.
- `view.switchWindows` lists the open documents (`{"documents": [{"id", "title", "path",
  "dirty", "active"}]}`); with `{"id": n}` it makes that document active and brings its window to
  the front.
- `file.closeDocument {"id"?, "force"?}` closes a document; its window closes with it unless it
  is the last window.
- `engine.execute`, `document.inspect` and `ui.render` take `doc`, the id of the document to work
  on, next to their other params. The command runs on that document and the active window stays
  active. A dialog or page the command opens (Save As, Print) opens in that document's window,
  which comes to the front:

```json
{"id": 2, "key": "…", "method": "engine.execute", "params": {"command": "text.insert", "params": {"text": "Hello"}, "doc": 3}}
```

`doc` left out, or `null`, is the active document. An id that names no open document, or a value
that isn't a whole number from 0 up, is an error and nothing runs. Window commands (`ui.*`) work
on the active window only: with the id of another document they are an error.

### The active window

Input and window methods go to the active window, the one focused last: `ui.click`, `ui.key`,
`ui.text`, `ui.wheel` and the other input methods (coordinates are that window's),
`ui.screenshot` (a PNG of that window), `ui.inspect` (its size), `ui.resize` and `ui.focus`. To
work in another window, make its document active first with `view.switchWindows {"id": n}`.

The `CharProps` and `ParaProps` that `format.set`, `para.set` and `styles.modify` take are listed
in [MCP server](mcp.md#character-and-paragraph-properties).

Positions are `{"story": "body" | {"part": n}, "path": [block, row, cell, block, …], "off": byteOffset}`.

### Keyboard and text input

`ui.key` and `ui.text` go to this window's focused widget even when another app is in front
(#2420). A focused Find field receives the text; a document receives it only while its canvas
owns keyboard focus. Shortcuts, dialogs and document protection still apply. These methods do
not activate the native window. `ui.focus` separately requests native activation.

The `{"queued": true}` reply acknowledges the input queue, not a completed edit. Input is applied
on subsequent UI frames. Inspect the resulting document or widget state before continuing with
an operation that depends on it; do not replay an edit simply because its effect is not immediate.

### Closing the document

`engine.execute` never shows the Save / Don't Save / Cancel prompt, so a command that would
discard unsaved work is **refused with an error** (the app keeps running and the edits stay);
save first (`file.save`), or pass `{"force": true}` to discard the changes and go ahead.

`file.close` closes the shown document, so it is refused while that document has unsaved
changes; the bare form `{"method": "file.close", "force": true}` (a method name that is a
command id) is guarded the same way, and only a boolean `true` counts as `force`. A document
without unsaved changes closes at once (#1268). Closing closes the document and its window, and
the window focused before it becomes the active one. The last window stays open with no page,
and `file.new` or `file.open` goes on from there (#1712); closing it doesn't quit the app. With
`doc` naming a document in the background, `file.close` closes that document and its window under
the same rule (unsaved changes need `{"force": true}`), and the active window stays active. A
macro's `file.close` step closes the document the window shows once the macro has run, as File ›
Close does: one with unsaved changes stays open, and the status line says why. It never quits
the app.

When another window shows the same document, `file.close` closes only the window it is sent to:
the document and its unsaved work stay open, so nothing is lost and the command is **not**
refused (#3274).

`file.new`, `file.open`, `file.recover` (with `restore`) and the mailings merges open their
result as **another document beside** the open ones and leave the unsaved one alone, so they
lose nothing and are never refused.

`file.closeDocument` closes the document its `id` names (the active one when the `id` is
absent), so it is refused while that document has unsaved work: changes that aren't written to
a file, a save the browser's picker hasn't finished, or a comment still being typed. Pass
`{"force": true}` to discard that work and close the document; only a boolean `true` counts as
`force`. The refusal and the Save Changes prompt ask about the same work, so they cannot
disagree (#3274).

`app.quit` closes every open document, so it is refused while any of them has unsaved changes:
the app keeps running and the work stays. Save first, or pass `{"force": true}` to discard the
changes and quit. Only a boolean `true` counts as `force`.

### Native printing

`engine.execute` can call `ui.printSettings` with `{}` to inspect the printer list, selected
printer, copies, duplex, grayscale, discovery/submission status and backend receipt/error.
`{"refresh":true}` starts background discovery; poll the same command until `discovering` is
false. Set `printer` to a discovered name, integer `copies` from 1 to 999, `duplex` to
`printerDefault`, `off`, `longEdge` or `shortEdge`, and boolean `grayscale`. Invalid updates are
rejected without changing earlier settings. `ui.printSubmit` explicitly submits the current
print-ready PDF without opening a dialog; inspect `ui.printSettings` until `busy` becomes false
and check `status`/`detail`. The browser keeps its own print hook. PDF export remains separate.

`ui.printSubmit` / `ui.print` also accept the upstream PDF page settings (`range`, `pages`,
`parity`, `copies`, `collate`). Native printing renders the selected pages once and passes
copies/collation to the spooler; the browser and `file.printPdf` keep copies in the PDF.
Omitting native `copies` uses the configured `ui.printSettings` value. `collate` defaults to
File › Print's setting; invalid ranges submit no job. `paper` (`none`, or a Layout › Size name
such as `Letter`, `Legal`, `A4`) is Scale to paper size (#2108): every page is scaled onto that
paper and centred, and native jobs ask the spooler for that `media`; the document's page size is
unchanged. `file.printPdf` takes it too; an unknown paper submits no job.

`engine.commands {"search":"print"}` lists the engine print commands and the app-owned
`ui.printSettings`, `ui.printSubmit`, and legacy `ui.print` alias, with current enablement
and their parameter documentation. Search is case-insensitive across IDs, labels, locations
and parameter descriptions. Listing commands never discovers printers, submits a job or opens
a dialog. Settings inspection is always available; submission becomes enabled when the native
printer is ready, or when the browser host provides its print flow.

### Physical print sheet layout

`ui.printSubmit`, `ui.print` and `file.printPdf` accept `pagesPerSheet`: the integer
1 (default), 2, 4, 6, 9 or 16, or `"booklet"` / `"bookletReverse"` for left/right binding.
These settings change only the print PDF, leaving document Page Setup unchanged.
Selection/range/parity are applied first. N-up fills cells in row order; booklet mode
pads the selected pages to a multiple of four and pairs outer/inner pages. Copies
repeat complete sheet plans, preserving blank slots at copy boundaries. Native copies
remain a spooler setting. Two-up and booklet sheets are landscape. Use duplex on the
short edge for folded booklets; this does not configure a printer's duplex capability.
The preview shows physical sheet sides, before copies, using the same sheet plan.
Explicit `paper` chooses the physical sheet; otherwise the first selected page supplies
its size. Independent Page Setup multiple-page modes (#2414) are not required.

## Shared documents

The window can share the document it shows with agents (and people's other programs) at once.
Each participant joins with a key of their own and gets a copy of the document of their own in
the app, which answers their connection on its own thread: an agent's command never waits for
the window's frame, and the person's typing never waits for an agent. Every copy, the window's
included, sends what each command changed to the document's room (`engine::room`), which puts
the changes in one order; an edit that clashes with a change the room took meanwhile comes back
to its copy, which makes it again after that change, from the same place. Nobody's edit is lost
unless another participant's change took its text away, and then its sender is told. The window
brings in what the others changed every frame.

With the window's key (each is a command of the window's too, for `engine.execute`, listed in
command search; the app's File › Share page runs them, and a session without the window refuses
them):

| Method | Params | Result |
|---|---|---|
| `room.share` | `name?` | shares the active document; the window joins its room as its owner, under `name`, else the author name (File › Options › User name), else `Owner`. `{"shared": true, "document", "version", "you", "participants", "agentFiles"}`: the document's id, the number of changes the room took, the window's participant and everyone in the room, each `{"id", "name", "role"}` and, for those who joined with a key, `"files"`: which files their commands can use now (`none`, `folders` or `all`, see [Files participants can use](#files-participants-can-use)); `agentFiles` is what File › Options › Agents gives agents. Sharing the same document again answers the same; another one is an error while one is shared |
| `room.invite` | `name`, `role?`, `files?` | a key (32 hex digits, random) for a participant called `name`, with `role` ([roles](#roles), `suggester` unless given): `{"key", "name", "role", "files"}`. A name is someone's in the room for good, even after they left, since their comments and tracked changes keep it: up to 128 characters, no control characters, compared without case. `files` (`none`, `folders` or `all`) gives the participant fewer files than File › Options › Agents gives agents, never more; left out (`null`), they get what the setting gives, whenever it changes |
| `room.revoke` | `key` or `name` | takes the invite back, by its key or by the name it was for (compared without case; the key shows once in File › Share): nobody joins with the key any more, and the copies that joined with it are out of the room (the room refuses their changes, and their connections end). `{"revoked": bool}` |
| `room.role` | `name`, `role` | gives the participant invited as `name` (compared without case) `role` from now on: the room holds their next change to it, their copies' next requests follow it, and a copy that joins with the key later has it. `{"changed": bool, "name", "role"}`. Only invited participants change role: the window keeps its own, and the built-in assistant's is chosen in its Assistant pane |
| `room.participants` | — | `{"shared": false}`, or what `room.share` answers, with `"invited"`: the invites not taken back, each `{"name", "role", "files", "joined"}` (`joined`: how many copies are in the room with its key now), never their keys |
| `room.stop` | — | stops sharing: the room closes and every participant's connection ends; the window keeps the document as it is, with every change the room took, whether it shows that document or another. `{"stopped": bool}` |

The window keeps the shared document up to date while it shows another one: every frame brings
in what the others changed, and every control request first gets what the room took since the
last frame, so a request addressed to the shared document (`doc`) reads, renders, saves and
exports it as the room has it. Commands addressed to it go through the window's copy, as the
window's own do while it shows the document.

Closing the shared document stops sharing too, however it closes (`file.close`, with `doc` or
without, or `file.closeDocument`), and so does quitting the app: once nothing is left unsaved,
the room closes before the app does (`app.quit` closes it first too). A close or a quit sees
every change the room took until then: an agent's edit since the window's last frame is unsaved
work, so a close or `app.quit` without `force` is refused for it, and the person's Close or quit
asks to save it. A macro can't close the shared document while it is
shared: its `file.closeDocument` step fails, saying to stop sharing first. A shared document is
never replaced by a file opened in its place, even a blank one: `file.open` opens beside it.
One document is shared at a time. Undo and Redo (`edit.undo`, `edit.redo`, `edit.undoTo`) take
back the steps of whoever runs them, the window's or a participant's own, and leave others'
changes in. When another participant changed what the step changed since (the same characters,
blocks, part or document field), the step stays and the command fails naming them
(`edit.undoTo` stops there and says why under `stopped`).

### Joining with an invite's key

Connect to the window's port and send the invite's key where the window's would go: top-level
`key`, or `auth` first. The connection's first request decides who answers it: from then on
the participant's own copy does, and the window never sees its requests. Later requests carry
the same key or none. The copy answers:

| Method | Params | Result |
|---|---|---|
| `engine.execute` | `command`, `params` | runs the command in the copy, held to the participant's role, and sends what it changed to the room until the room took all of it (up to 1,024 changes at a time, so a long macro's go in several). The command's result; an error when the room sent the edit back and it couldn't be made again because another participant's change took its text away (the error names the command), or when others' changes sent its changes back four times in a row and the room hasn't taken them yet (they stay in the copy and go first with the next request; closing the connection drops them) |
| `document.inspect` | `text?` | the document as the copy has it, with what others changed |
| `engine.commands` | `search?` | every command, as for the window; `enabled` says whether this participant may run it now |
| `ui.render` | `page?`, `scale?` | a page of the document as base64 `png`, as for the window; `path` is refused (`file.exportPng` writes a page to a file the participant may write) |
| `ui.parity` | — | as for the window |
| `room.participants` | — | `{"you", "participants", "version", "files"}`: the caller, everyone in the room, the version of the document the copy has, and which files the caller's commands can use now (`none`, `folders` or `all`; with `folders`, also `"folders"`, each `{"path", "write"}`) |

Any other method name is a command id, as for the window. Each request the copy doesn't refuse
first sends the participant's changes the room hasn't taken yet, if an earlier command left
some, and brings in what others changed since. When one of those changes couldn't be made again, the request fails
with the error that says so (naming its command) and does nothing else, so it can be sent
again; a command also doesn't run while the room still hasn't taken them. `doc` may name the
shared document only. Commands that work on files,
other documents or windows (`file.*` but the exports, `view.switchWindows`, `ui.*`), the window's
methods (`ui.click`, `ui.screenshot`…, `app.quit`) and the room's other methods (which are the
owner's) are refused. Their commands reach only the files the person lets agents use, or fewer
(see [Files participants can use](#files-participants-can-use)). `document.changes` follows what changed, by the window and others alike, in the
copy's own versions.

When the invite is taken back or sharing stops, the connection's next request gets an error
that says so, and the app closes the connection. An invite's key is a wrong key while no
document is shared.

`wordcraft-cli mcp --connect 127.0.0.1:7981` with `WORDCRAFT_CONTROL_KEY` set to the invite's
key gives an agent its copy through MCP's tools (see [MCP](mcp.md#shared-documents)).

### Files participants can use

Which files the commands of participants who joined with a key can read and write is the
person's choice, in File › Options › Agents (`file.options` `agentFiles` and `agentFolders`),
saved with the app's settings:

| `agentFiles` | Their commands can use |
|---|---|
| `none` (the default) | no file: a command that reads or writes a file it names (mail merge's recipients `path`, a picture's, an export's) is refused |
| `folders` | the files in `agentFolders` (at most 32, each a full path to an existing folder on this computer, not a network folder): they read in each, and write only in those with `"write": true` |
| `all` | any file on this computer |

- That is the most any participant gets. An invite's `files` gives its participant fewer (`none`,
  or `folders` while the setting is `all`), never more: while the setting is `none`, no
  participant's command reaches a file, whatever its invite says, so choosing None stops every
  agent at once.
- It is asked at each request, so a change holds from each participant's next request on. When it
  narrows (a narrower choice, a folder taken out or made read-only), when the invite is taken back
  or when sharing stops, a request running then (a long macro, say) reaches no file from then on.
- A path must be a full one on this computer: a relative path or a network path
  (`\\server\share`, `//server/share`) is refused, and so is a path outside every folder, by its
  spelling, before the file system is asked anything about it. Links are followed: one that leads
  out of a folder is outside it, and on Windows one that leads to another computer, or a drive
  mapped to one, is refused too. On macOS, Linux and BSD a network folder mounted as an ordinary
  folder is reached like any other, so with `all` agents reach it.
- WordCraft's own settings folder, which holds its preferences, its recovery copies and the
  window's key, is never in reach, even with `all`.
- Every role exports the document where it may write: `file.exportPdf`, `file.exportPng` and
  `file.printPdf` run in every copy, each with a `path` (there is no window to ask), and make a new
  file, never replacing one: it is created in one step once the export is made, so two exports
  racing to one name never replace each other, and only if the participant may still write there. Saving (`file.save`, `file.saveAs`) stays the window's: it stamps the
  document and gives it a path. An owner's commands on the application's settings (fonts,
  language packs) reach no file.
- No participant's copy runs `file.options`, whatever its role: which files agents can use is set
  at the window. Commands sent with the window's key are the person's: they use every file the
  person can, and may change the setting.
- The web app reaches no files, so it has no such setting.

### The room's own port

The desktop app also listens for the shared document's room alone, on `127.0.0.1` and a port
the system picks, whether or not it runs with `--control`. The built-in assistant's agents reach
their copies there: an agent gets `wordcraft-cli mcp --connect 127.0.0.1:<port>` with its
invite's key in `WORDCRAFT_CONTROL_KEY`, and nothing else.

It speaks this protocol, and only invites' keys open it: a connection's first request must carry
one, and from then on it is served as above. Every connection with the same key works on the
same copy, one request at a time, so an agent that connects for each request
(`wordcraft-cli call` in its shell) finds what it selected still selected, and stays one
participant; the port lets the copy go once the invite is taken back or sharing stops. The window's key opens nothing there (the port
doesn't know it, and no key file names the port), nor does any other key, and while no document
is shared no key does: the request gets `{"ok": false, "error": "unauthorized"}` and the
connection closes. The limits are the control channel's: a line over 1 MiB, a line that isn't
UTF-8 or JSON, or no request with an invite's key within 30 s of the connection being accepted
closes it. At most 64 connections are served at once. Each response has a two-second total
write deadline, including clients that read only a few bytes at a time. A failed write closes
the connection; revocation and room closure also end idle authenticated reads. These limits
bound resource use; a continuous local connection flood can still deny availability.

### Roles

| Role | May |
|---|---|
| `viewer` | read the document and move their own caret, selection and view; changes nothing |
| `commenter` | also add comments and replies, and edit and delete their own |
| `suggester` | also edit, always as tracked changes in their name, which others accept or reject; they withdraw their own |
| `editor` | also edit directly, and accept and reject tracked changes |
| `owner` | also change the application's settings in their copy |

A participant's copy undoes and refuses a command that changes more than their role lets them,
and the room refuses such a change from any copy.
