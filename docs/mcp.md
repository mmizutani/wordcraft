# MCP server

WordCraft speaks the [Model Context Protocol](https://modelcontextprotocol.io) over stdio, so
Claude and other agents can write and edit documents.

```sh
wordcraft-cli mcp                          # headless session (no window)
wordcraft --control 7981 &                 # or: drive the running app…
wordcraft-cli mcp --connect 127.0.0.1:7981 # …including clicks, keys and screenshots
```

Claude Code: `claude mcp add wordcraft -- wordcraft-cli mcp`.

With `--connect`, the bridge sends the app's control key, which it reads from
`WORDCRAFT_CONTROL_KEY` or from the key file the app writes at start. It sends the key only to
loopback addresses; see [Keys](control-protocol.md#keys). The bridge serves even when the app
isn't open yet: tool calls return "not reachable … will retry on each call" and succeed once the
app's port opens (#4078), so a client that starts before WordCraft doesn't lose the server for
its whole session.

## Scripts from the command line

`wordcraft-cli run --script job.json` (`--script -` reads standard input) runs the commands in a
file, in order, exactly like repeated `--cmd`, and then saves. Use it to generate a DOCX and its
PDF in one run without a window:

```json
{"template": "sample",
 "commands": [{"command": "select.text", "params": {"text": "Membership"}},
              {"command": "format.bold"},
              "caret.docEnd"],
 "save": ["out.docx", "out.pdf"]}
```

`file` (a document to open) and `template` are optional, and `--file` / `--template` on the command
line win. A command is `{"command": id, "params": {…}}` or just the id. `save` is a path or a list;
each output's format follows its extension. Relative `file` and `save` paths resolve against the
script's folder (the working folder for stdin). `--cmd` flags run after the script's commands and
`--save` after its outputs. The run starts with one open document. `file.new` and `file.open`
open another one and make it active; later commands and the saves work on the active document.
`view.switchWindows` lists the open documents (with `--print` their ids show), and
`view.switchWindows={"id": n}` makes one active. A failing command stops the run with `command N (id): …` and exit
status 1, and nothing is saved. Every output is checked (known extension, folder exists) before the first is written. Paths inside command params (a picture to insert, say) are relative to the working folder, not the script's. Scripts are capped at 4 MiB.

## Tracked changes by an agent

Tracked changes and comments are recorded under the session's user name ("WordCraft User" by
default). Name the agent so reviewers can tell its edits from people's: start the headless server
with `wordcraft-cli mcp --author "Claude (copyedit)"`, or run
`file.setAuthor {"name": "Claude (copyedit)"}`. Turn tracking on explicitly with
`review.trackChanges {"value": true}`; without `value` it toggles, which turns tracking *off* in
a document that was saved with it on. `wordcraft-cli run` takes `--author` too.

## Confining file access

```sh
wordcraft-cli mcp --automation-read-root /work/project --automation-write-root /work/project/out
```

```json
{"mcpServers": {"wordcraft": {"command": "/abs/path/target/release/wordcraft-cli",
  "args": ["mcp", "--automation-read-root", "/work/project", "--automation-write-root", "/work/project/out"]}}}
```

The two flags (VectorCraft, PhotoCraft and the other Craft apps take the same ones, so one client
config works across the suite) keep the agent to the folders you give it:

- Every file a command reads must lie inside the **read root**: `open_document` and `file.open`,
  `file.newFromTemplate`, `insert.textFromFile` and `insert.object`, `insert.picture` and
  `picture.change` with a `path`, `mailings.recipients` with a `path` (csv, tsv, txt, xlsx, xlsm,
  ods), `review.compare` (`path` and `original`), the templates and documents `tools.updateStyles`,
  `styles.organizer` and `styles.copyFrom` read, and the folders `file.options` takes
  (`dictionariesFolder`, `userTemplatesFolder`). The pictures an opened HTML file names are read
  from beside that file and must be inside the read root too, even when the file was opened through
  a link (#3736).
- Every file a command writes must lie inside the **write root**: `save_document`, `file.save`,
  `file.saveAs`, `file.rename` (the file and its new name), `file.exportPdf`, `file.exportPng`, `file.printPdf`,
  `file.saveTemplate`, and `mailings.finish` with a `path`. `font.add` and `font.remove` change the
  user font folder, so they are refused when it is outside the write root (#3692), and so are
  `proof.installPack` and `proof.removePack`, which change the font store's `language-packs`
  folder. A save replaces
  the name it is given, so that name must be inside the write root as well as what it leads to:
  a link outside the root pointing in is refused (#3735).
- Read and write access are separate. Give the same folder twice for both; a root left out grants
  none of its access (with only `--automation-read-root`, nothing can be written to disk). Without
  either flag nothing changes.
- A path is made absolute against the server's working directory, then its links are followed: a
  file (or folder) link that leads out of a root is outside it. For a file that doesn't exist yet,
  its nearest existing folder's links are followed and the rest must be plain names, so `..` below a
  folder that doesn't exist is refused, as is a link that leads nowhere (writing through it would
  create its target). The result is compared with the root by whole folder names: `/work2/x` is not
  inside `/work`. On Windows case is ignored, UNC paths (`\\server\share\...`) are resolved like
  drive-letter ones, and device names (`CON`, `NUL`, `COM1`...) are refused.
- A refused path is an ordinary tool error the agent can read and correct, for example
  `automation path rejected: outside the write root /work/project/out: /etc/x.docx` or
  `automation filesystem access is not granted: write authority is absent: /work/project/out/x.docx`.
- The roots must be existing folders; a missing one stops the server with an error. Each flag is
  given once (`--flag <dir>` or `--flag=<dir>`).

The flags confine the headless server; `--connect` with them is an error, since the server would
otherwise talk to an app that isn't confined. WordCraft's desktop app has no equivalent flags yet,
so a running app (`wordcraft --control`) is not confined.

What the roots do not cover: commands that take no path (`text.insert`, `execute` of anything that
works on the open document) can still read and change that document, and `get_text` and
`inspect_document` return it. The Zotero commands talk to a local Zotero over a socket, not to files.
Anything that reads or writes a path goes through one place in the engine (`io::open_path_with`,
`io::save_path_with`, `io::read_file`, `io::write_file`); a test keeps command modules from
touching the file system on their own.

The checks hold for every thread of the server process. A path is checked, then opened: another
program that swaps a link in between can still win that race, so the roots keep an agent to the
files it was given rather than guarding against other software on the computer.

## Language packs

Spelling and hyphenation for languages other than English come as Language packs, installed into
the suite's font store (see [installing languages](spelling-dictionaries.md#language-packs)):

| Command | Params | Returns |
|---|---|---|
| `proof.packs` | `{"refresh"?: bool}` (fetch the pinned index again) | `{"packs": [{"locale", "name", "size", "licence", "class", "source" (upstream), "archive", "spelling", "hyphenation", "installed"}], "languages": [{"tags", "pack", "installed", "size", "proofing" (built_in, installed, own_dictionary, system_dictionary, install, grammar_built_in, not_available), "default"}] (Options › Language › Your languages), "installed": [...], "installing": [{"locale", "done", "total"}], "refreshing", "errors": {locale: message}, "network", "available"}` |
| `proof.installPack` | `{"locale": "nl"}` | `{"locale", "name", "licence", "installed": true}` (`"already": true` when it was); with `--connect`, `{"locale", "started": true}` |
| `proof.removePack` | `{"locale": "nl"}` | `{"locale", "installed": false}` |

None opens anything. Downloading is the font store's single consent setting: an agent can install
only while the person has turned it on in the app (File › Options › Language: Install or Add a
language…), and can never turn it on itself; otherwise `proof.installPack` fails with
*downloading is turned off*. `refresh` likewise needs it on. Every archive is checked against the
sha256 in the pinned index before it is unpacked. With `--connect` the commands act on the running
app's packs, and an install or a `refresh` runs in the background so the window keeps working: the
command returns `"started": true` at once, and `proof.packs` shows it under `installing` (or
`refreshing`), then under `installed`, or its failure under `errors`. A removal waits until that
pack's install ends. Headless, they act on the same font store folder and return when done. The
web build has no packs.

A click or key sent with `click`/`key` on Add a language…, Install or Refresh never turns
downloading on either: only the person's own click does; the app shows why instead.

The assistant's consent is kept the same way. With `--connect`, `execute` reads File › Options ›
Assistant › Allow what agents ask when no one can answer (`assist.allowUnattended`) and can turn
it off, never on. `click` and `key` never turn it on, never answer the Assistant pane's permission
or sign-in questions, and a prompt they send from the pane is a program's, not the person's: its
agent's requests are answered per that setting, refused unless the person allowed them.

Installing reads the pinned index of craft-i18n's language-packs-2 release (75 languages). For
testing packs before they are released, set `CRAFT_LANGUAGE_PACK_INDEX` to another index (a URL or a path
to an `index.json`) when starting WordCraft or `wordcraft-cli`. That index isn't pinned, so the
sha256 of each archive only matches what that index says: it catches a broken download, not a
changed pack. `proof.packs` then returns `"index_override": "<url or path>"`, and the app shows the
test source in File › Options › Language and on the bar's Install.

## Tools

| Tool | What it does |
|---|---|
| `list_commands` | every command, with its scope and the params that are places (filter with `query` and `scope`; see [Command scopes](#command-scopes)) — over 650 of them |
| `execute` | run one command: `{command: "insert.table", params: {rows: 3, cols: 4}}` |
| `batch` | run several commands in order |
| `new_document` | blank, sample, letter, resume, report; opens beside the open documents |
| `open_document` / `save_document` | docx, doc (open only), odt, rtf, html, md, tex, txt, json; save also pdf, png |
| `list_documents` | the open documents: `id`, `title`, `path`, `dirty`, `active` |
| `type_text` | type at the caret (`paragraphs: true` splits lines into paragraphs: one undo
  step; each break is the Enter it stands for, so AutoFormat and AutoCorrect run and a
  one-line control refuses; at most 10,000 lines — the cap is `text.insert`'s own, so a
  control-channel or CLI call is held to it too) |
| `select_text` | select the n-th occurrence of some text |
| `get_text` / `inspect_document` | read the document back (verify without screenshots); `inspect_document` gives each block's `id` and the document's `version` |
| `get_changes` | what changed after a version, by people or other agents (see [Following changes](#following-changes)) |
| `render_page` | a page as PNG |
| `parity` | feature coverage |
| `screenshot`, `click`, `key`, `ui_inspect` | app only (`--connect`) |
| `room` | share the document the app's window shows with other agents, and hand out their keys: `{action: "share" \| "invite" \| "revoke" \| "role" \| "participants" \| "stop", name?, role?, files?, key?, remote?, expiresIn?, uses?}` (app only; see [Shared documents](#shared-documents)) |

### Several documents

`new_document` and `open_document` (the commands `file.new` and `file.open`) open another
document beside the ones already open and make it the active one; their reply carries its `id`.
`open_document` replaces a blank document nobody has used, and a file that is already open becomes
active again (`alreadyOpen`). `list_documents` lists the open documents with their ids.

Tools work on the active document. `execute`, `batch`, `type_text`, `select_text`, `get_text`,
`inspect_document`, `get_changes`, `render_page` and `save_document` also take `doc`, the id of another open
document: the tool works on that document and the active one stays active.

```json
{"name": "execute", "arguments": {"command": "text.insert", "params": {"text": "Hello"}, "doc": 3}}
```

An id that names no open document, or a value that isn't a whole number from 0 up, is a tool error.
`execute` with `view.switchWindows {"id": n}` makes a document active, and `file.closeDocument
{"id": n, "force"?: true}` closes one (unsaved changes need `force`). Through `--connect`, `doc`
reaches the app's control channel (see [Open documents](control-protocol.md#open-documents)); the
app's window keeps showing the document it showed. `screenshot`, `click` and `key` act on the
window, so they work on the document on screen.

`insert.picture` and `insert.icon` leave the caret after the new picture, so typing next doesn't
replace it; Picture Format commands (`picture.*`, `arrange.*`) still act on it.

Resources: `wordcraft://document` (inspect) and `wordcraft://commands`.

The acceptance test `crates/mcp/src/tests.rs` writes a formatted document using MCP only.

### Following changes

An agent that works beside people (or other agents) reads the document once and then follows what
changes, instead of reading it again. `inspect_document` gives the `version` it read and an `id`
for each block; `get_changes {"since": version}` gives, oldest first, each change made after it:

```json
{"document": 3, "version": 13, "changes": [
  {"version": 12, "edits": [{"in": {"story": "body"}, "paragraph": "5f0c…", "at": 5, "deleted": 0,
    "text": " world"}]},
  {"version": 13, "edits": [{"in": {"story": "body"}, "after": "5f0c…", "before": "9a41…",
    "removed": [], "inserted": ["c27e…"], "at": 0, "deleted": 0, "text": ""}]}
]}
```

An edit inside a paragraph (typing, deleting or formatting characters, or changing the
paragraph's own properties) names the list it is in (`in`: the body, a header, note or text box,
or a cell `{"cell": {"table", "row", "col"}}` of a table), the paragraph's id (`paragraph`; it
keeps its id) and the characters it replaced, as byte offsets in its text: `deleted` bytes from
`at`, `text` put there (U+FFFC for an object; formatting a word gives the same word back, and
places in it keep their offsets), and
`properties: true` when its alignment, style or other paragraph properties changed. Any other
edit names the list, the ids of the blocks it took away and put in (Enter at the end of a
paragraph adds one after it; Enter inside one takes it away and puts back its two halves, the
first keeping its id; joining paragraphs takes them away and puts one back), the blocks around
it (`after`, `before`) and the text splice on those blocks' text, with U+2029 between
paragraphs and U+FFFC for a table. `tables` lists tables whose properties changed,
`parts` the notes, comments, text boxes and headers added, removed or changed, and `fields` the
other things that changed (`styles`, `numbering`, `settings`, `comment 3`, `revisions`…). Undo and
redo are changes too. `full: true` adds each change whole (`change`), as another copy of the
document applies it (`Document::apply`). At most `limit` changes come back (100 unless given);
`more: true` says to ask again from the last version given. Leaving `since` out gives the current
version and no changes. A version older than the document keeps (its last 1,024 changes, about
64 MB of them) is an error: read the document again. Through `--connect` this is the app's
`document.changes` command, so an agent sees what the person types in the window.

### Command scopes

Each command `list_commands` gives says what it touches (`scope`), which decides who may run it in
a shared document, and which of its params are places in the document (`positions`):

```json
{"id": "select.range", "label": "Select Range", "params": "{\"anchor\": Pos, \"focus\": Pos}", "mutates": false,
 "scope": "selection", "positions": {"anchor": "pos", "focus": "pos"}, …}
```

| `scope` | What the command touches | For example |
|---|---|---|
| `document` | the document everyone sharing it sees: its text, formatting, objects, styles, settings and properties; reading any of it | `text.insert`, `format.bold`, `review.comments`, `document.changes`, `file.properties` |
| `selection` | what is the participant's own while they edit: the caret and selection, Find, the clipboard, the pen in hand | `caret.set`, `select.text`, `edit.find`, `edit.copy` |
| `view` | how the participant sees the document: views, zoom, panes and dialogs, Read Aloud | `view.zoom`, `review.markup`, `styles.pane` |
| `file` | files and the other open documents: opening, saving, exporting, printing, versions; and commands that make a new document or switch to another | `file.save`, `view.switchWindows`, `mailings.finish`, `review.compare` |
| `app` | the application's settings and data, which every document shares | `file.options`, `tools.autocorrect`, `font.add`, `review.addToDictionary` |
| `ui` | the app's own window commands (through `--connect`) | `ui.printSubmit` |

A scope covers everything the command can do: a `selection` or `view` command never changes the
document (a test runs each one to check), and one that can make or switch to another document is
`file`, even when it usually works on this one. In a shared document, every participant's copy
runs `document`, `selection` and `view` commands (what a `document` command changes is held to
their role) and the exports (`file.exportPdf`, `file.exportPng`, `file.printPdf`, to new files
only), only the owner's runs `app` commands too, but never `file.options`, and none runs the other
`file` commands or `ui` commands: those work on the app's files, other documents and windows (see
[Shared documents](#shared-documents)). Paths are limited apart from scopes, by the roots above
and, in a shared document, by the files the person lets agents use
([Files participants can use](control-protocol.md#files-participants-can-use)).
`list_commands {"scope": "document"}` lists one scope's commands.

`positions` gives each param that names a place in the document, and its form: `pos` (a position,
`{story, path, off}`), `list` (a list holding positions) or `path` (a block's path). Page
coordinates (`page`, `x`, `y`) aren't places in the text: they depend on one copy's layout. A
command written against an older version can have its places moved through the changes made since
(`ChangeSet::map`), so they still point at the same text.

### Shared documents

Agents and the person at the app's window can edit one document at once. Through the window's
key (`--connect`), the `room` tool shares the document the window shows and hands out keys, each
for a name and a role:

```json
{"name": "room", "arguments": {"action": "share"}}
{"name": "room", "arguments": {"action": "invite", "name": "Fact checker", "role": "suggester"}}
{"name": "room", "arguments": {"action": "invite", "name": "Proofreader", "role": "suggester", "files": "none"}}
```

The invite answers `{"key": "…", "name": "Fact checker", "role": "suggester", "files": null}`. Another agent
joins with that key in place of the window's:

```sh
WORDCRAFT_CONTROL_KEY=<the invite's key> wordcraft-cli mcp --connect 127.0.0.1:7981
claude mcp add fact-checker -e WORDCRAFT_CONTROL_KEY=<the invite's key> -- wordcraft-cli mcp --connect 127.0.0.1:7981
```

Its tools then work on a copy of the shared document of its own, inside the app: `execute`,
`batch`, `type_text`, `select_text`, `get_text`, `inspect_document`, `get_changes`,
`render_page`, `list_commands`, `parity` and `room {"action": "participants"}`. Its caret and
selection are its own: the others see where they are once a command of its own moved them or
edited, and `room {"action": "participants"}` says where everyone's are (`selection`, anchors as
`select.range` takes them, with `story`, `"body"` or `{"part": n}`, and `index`, as `inspect_document` numbers blocks; see
[Where everyone is](control-protocol.md#where-everyone-is)), so it can keep clear of where a
person types. Its commands are held to its role (see
[Roles](control-protocol.md#roles)): a suggester's edits are tracked changes in its name. Each
call first brings in what the others changed since, and returns once the room took all of the
agent's edit, a long macro's too; an edit whose text another participant took away meanwhile
comes back as an error that names the command, and so does one the room still hasn't taken
after others' changes sent it back four times in a row (it stays in the agent's copy and goes
first with its next call). A call that finds such an edit lost does nothing else and says so,
so the agent can make it again. `edit.undo` and `edit.redo` take back the agent's own steps and leave
everyone else's edits in; when someone changed what the step changed since, they fail and say
who. `get_changes` shows the window's changes and other agents' alike. Files,
other documents and the window (`open_document`, `save_document`, `list_documents`,
`screenshot`, `click`, `key`, `ui_inspect`) and the room's other actions are the owner's, but
`save_document` with a `.pdf` or `.png` path exports a copy of the shared document. `execute`
with `file.exportPdf` or `file.exportPng` and no `path` hands the PDF or page back in the reply
(base64 `pdf` or `png`, up to 32 MB), for an agent with no file to write to.

An agent on another machine (a sandbox) is invited with
`{"action": "invite", "name": "Sandbox", "remote": true}`: never the owner, no file unless
`files` names some, and a key that stops working after a day unless `expiresIn` (seconds) says
otherwise; `uses: 1` lets it join once. It will reach the room through a relay this computer
dials out to, with no tunnel to run (#75, #76); see
[Agents on other machines](control-protocol.md#agents-on-other-machines). Only an invite's key
goes to a sandbox, never the window's.

Which files the agent's commands can use is the person's choice, in the app's File › Options ›
Agents: none (the default), only the folders listed there, each read-only or read and write, or
any file on this computer. An invite's `files` (`none`, `folders` or `all`) gives its
agent fewer, never more, so None there stops every agent at once. A command that reads or writes
a file outside that (mail merge's recipients `path`, a picture's, an export's) is refused with an
error that says why; paths must be full ones, never on another computer, and WordCraft's own settings
folder is never in reach. An export makes a new file in a folder the agent may write in, never
replacing one. A change to the setting holds from the agent's next call on, and when it narrows,
a call running then reaches no file from then on. `room {"action": "participants"}` tells the
agent which files it can use (`files`, and `folders` with `folders`); no agent changes the
setting, since `file.options` is refused in its copy (see
[Files participants can use](control-protocol.md#files-participants-can-use)).

The owner changes an invited agent's role with `{"action": "role", "name": "Fact checker",
"role": "editor"}`: its next call is held to the new one. Revoking the key (`{"action": "revoke",
"key": …}`, or `"name"` for the name it was invited under) or stopping sharing ends the agent's
connection: its next call is an error that says why. The app's File › Share page does all of this
with the same commands. See [Shared documents](control-protocol.md#shared-documents) for the control
channel's side.

## Character and paragraph properties

`format.set {"props": CharProps}`, `para.set {"props": ParaProps}`,
`styles.modify {"style": …, "chr"?: CharProps, "para"?: ParaProps}` and the `para` of
`table.newStyle` / `table.modifyStyle` take these two objects. Every field is optional: a field
left out keeps what the style chain gives. A field name the object doesn't know is ignored, not
refused, so check the spelling. Lengths are points. A colour is a hex string `"RRGGBB"` or
`"#RRGGBB"`, or `[r, g, b]` (three numbers 0–255); it reads back as `[r, g, b]` (#865).

```json
{"command": "styles.modify", "params": {"style": "Heading1",
  "chr": {"font": "Georgia", "size": 18, "bold": true, "color": "1B1611"},
  "para": {"spaceBefore": 12, "lineSpacing": {"rule": "multiple", "value": 1.15}, "keepNext": true}}}
```

### CharProps

| Field | Type | Values |
|---|---|---|
| `style` | string | character style id |
| `font` | string | font name |
| `size` | number | points |
| `bold`, `italic`, `strike`, `doubleStrike`, `caps`, `smallCaps`, `hidden`, `outline`, `shadow`, `emboss`, `engrave`, `noProof` | bool | |
| `underline` | string | `none`, `single`, `words`, `double`, `thick`, `dotted`, `dash`, `dotDash`, `dotDotDash`, `wave`, `doubleWave` |
| `underlineColor` | colour | |
| `color` | `"Auto"`, a colour, or `{"Rgb": colour}` | `Auto` (also `"auto"`) is black on light backgrounds, white on dark; read back as `"Auto"` or `{"Rgb": …}` (#865) |
| `highlight` | string | `none`, `yellow`, `brightGreen`, `turquoise`, `pink`, `blue`, `red`, `darkBlue`, `teal`, `green`, `violet`, `darkRed`, `darkYellow`, `gray50`, `gray25`, `black`, `white` |
| `shading` | colour | background fill |
| `border` | Border | character border; `"style": "none"` is explicitly no border |
| `vertAlign` | string | `baseline`, `superscript`, `subscript` |
| `spacing` | number | character spacing, points: positive expands, negative condenses |
| `scale` | number | horizontal scale, percent (100 = normal) |
| `position` | number | raised (positive) or lowered (negative), points |
| `kern` | number | kern fonts at or above this size, points; 0 = off |
| `textEffects` | object | WordArt `fill`, `outline`, `shadow`, `glow`, `reflection` (see `crates/doc/src/wordart.rs`) |
| `lang` | string | BCP 47 language tag (proofing) |
| `rtl` | bool | right-to-left run |
| `cs` | bool | format the whole run with the complex-script properties |
| `fontCs`, `fontEa` | string | complex-script font; East Asian font |
| `sizeCs` | number | complex-script size, points |
| `boldCs`, `italicCs` | bool | complex-script bold, italic |
| `langBidi` | string | BCP 47 language of complex-script text, e.g. `fa-IR` |
| `link` | string | hyperlink target: a URL, or `#bookmark` |
| `ins`, `del`, `fmtChange` | | Track Changes bookkeeping (revision indexes) |

`format.set` also applies `size`, `bold` and `italic` to `sizeCs`, `boldCs` and `italicCs` when
those aren't given; `styles.modify` sets only what it is given.

A **Border** is `{"style": …, "width": pt, "color": colour | null, "space": pt}` (`space` is
the distance from the text). Missing fields are `none`, 0, no colour and 0, so a border needs a
`width` above 0 to show. `style` is one of `none`, `single`, `double`, `dotted`, `dashed`,
`thick`, `triple`, `dotDash`, `wave`.

### ParaProps

| Field | Type | Values |
|---|---|---|
| `style` | string | paragraph style id |
| `align` | string | `left`, `center`, `right`, `justify`, `distribute` (`left` is the start edge: the right in a right-to-left paragraph) |
| `indentLeft`, `indentRight` | number | points |
| `indentFirst` | number | first-line indent, points; negative = hanging |
| `indentLeftChars`, `indentRightChars`, `indentFirstChars` | number | the same indents in characters; a non-zero value wins over the one in points, 0 cancels an inherited one |
| `spaceBefore`, `spaceAfter` | number | points |
| `lineSpacing` | object | `{"rule": "multiple", "value": 1.15}` (1 = single, 2 = double), `{"rule": "atLeast", "value": pt}`, `{"rule": "exactly", "value": pt}` |
| `contextualSpacing` | bool | ignore space before/after between paragraphs of the same style |
| `mirrorIndents` | bool | left and right indents swap on even pages |
| `keepNext`, `keepLines`, `pageBreakBefore`, `widowControl` | bool | |
| `outlineLevel` | integer | 0–8 = Level 1–9, 9 = body text |
| `numbering` | object | `{"num": n, "level": n}`, both required; `level` counts from 0; `num` 0 removes numbering a style gives |
| `tabs` | array | `[{"pos": pt, "align"?: "left\|center\|right\|decimal\|bar\|clear", "leader"?: "none\|dot\|hyphen\|underscore\|middleDot"}]`; replaces the inherited stops, `clear` removes an inherited one at `pos` |
| `shading` | colour | background fill |
| `borders` | object | `{"top", "left", "bottom", "right", "between"}`, each a Border (`between`: between paragraphs; `inside_v` is used by tables) |
| `suppressHyphens`, `suppressLineNumbers` | bool | |
| `bidi` | bool | right-to-left paragraph |
| `dropCap` | integer | lines to drop, 0 = none |
| `kinsoku`, `wordWrap`, `overflowPunct`, `autoSpaceDE`, `autoSpaceDN`, `snapToGrid` | bool | East Asian typography; unset = on |
| `topLinePunct` | bool | East Asian typography; unset = off |
| `numChange`, `fmtChange` | | Track Changes bookkeeping |

`para.set` also takes `"onPage": {"align"?: "left|center|right|justify|distribute", "left"?: pt,
"right"?: pt}`: the alignment and indents as seen on the page, which each selected paragraph turns
into its own start and end once `props` (a `bidi` in it, say) is applied. The Paragraph dialog sends
only the fields that changed this way, as one command and so one undo step.
