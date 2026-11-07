# Co-editing: people and agents edit one document at once

> **Last reviewed:** 2026-11-07 · **Last updated:** 2026-11-07 · **Change:** minor (presence as built (#32, #71, #72): places as anchors moved with the text, colours, `room.participants`, others drawn in the window, avatars, following; the assistant's known limit and the A1 rows updated); previously minor (block ids kept in Word files (#38, #69): `w14:paraId` and an ignorable `wc:blockId`, read back on open; A5 rows updated); previously minor (File › Share (#33) and remote-safe invites (#74) as built: the page, `room.role`, revoking by name, invites that expire, join a set number of times or are for another machine; known limits and milestones follow); previously minor (the built-in assistant in the Assistant pane as a co-editor: who takes part, how a conversation joins and leaves, roles in the pane, tracked changes in its name, editing at the same time as the person and other agents, what comes next for it); previously major (first version: the design as built on `main` through A1.4 and the agents' file setting, what comes next by milestone, and the decisions; presence is a placeholder until A1.5 lands, #40) · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

This is the design of co-editing in WordCraft: several people and AI agents edit one document at
the same time, each change carries its author, and nothing is lost or corrupted. The agents are
both the built-in assistant in the Assistant pane, which edits beside the person in real time
as a suggester, an editor or another role, and outside agents that join over MCP or the control
channel. It is the
reference the co-editing pull requests cite. It describes what is built on `main`, what comes
next, and the decisions behind both. Work is tracked in issue #28 and its sub-issues; the gap is
#22 in [gaps.md](gaps.md).

Related: [architecture.md](architecture.md) (data flow), [control-protocol.md](control-protocol.md#shared-documents)
and [mcp.md](mcp.md#shared-documents) (how agents join), [assistant.md](assistant.md) (the
built-in assistant, which joins as a participant).

## Who edits together

| Participant | How it joins | Its copy | Its role |
|---|---|---|---|
| The person, in the app's window | The window shares the document it shows (File › Share, `room.share`, or the Assistant pane's first prompt) | The window's own session, lent to the room's owner copy | Owner |
| The built-in assistant, in the Assistant pane (ChatGPT, Claude Code, Codex, Pi or a custom ACP agent) | Each conversation takes an invite of its own from the window's room, under its backend's name | A hosted copy the conversation holds (ChatGPT), or one its agent process reaches through the room's port (ACP) | Suggester by default; viewer, commenter or editor when the person picks one in the pane or in File › Options › Assistant; never owner |
| Outside agents (any MCP client, or a program on the control channel) | An invite's key from the owner (File › Share › Invite, or `room.invite`), then `wordcraft-cli mcp --connect` or the control port | A hosted copy on their connection's thread | The invite's: suggester unless the owner chose another |

All three go through the same room, hub and rules: the assistant is not a special path. What
differs is only where its copy runs and who starts it.

## The design in one paragraph

Every participant, person or agent, edits a full copy of the document of its own, and one hub
per document puts every change in a single order. An edit shows at once in its author's copy.
The copy sends the command's change (`doc::ChangeSet`) with the version of the document it was
made on. The hub accepts a change made on its latest version as it is, moves one made on an
earlier version past the changes it accepted since (`ChangeSet::rebase`), and sends back one
that clashes with them. The author's copy then makes the same edit again, by running the same
command from the same places, moved through the other change. People never see a conflict
dialog. Agents join as named participants with roles through invite keys, and a suggester's
edits are tracked changes in its own name.

## Why one hub, and not a CRDT

Decided by Minoru on 2026-11-05: one authority per document is fine for now, as long as editing
feels as seamless as Google Docs and Notion. Google Docs and Figma also order edits on one
server; Notion uses a CRDT only for pages kept offline.

The reasons come from the code:

- **No corruption by construction.** The hub applies a change only to the content it was made
  on (`Document::apply` checks block ids and content fingerprints), so every state the hub holds
  is one WordCraft's own commands produced.
- **Ids that merge badly.** Tracked changes point into `Document.revisions` by index, and new
  comments, parts and lists take the largest id plus one. Replicas that merge on their own would
  collide and misattribute changes. With one order, they can't.
- **DOCX fidelity.** The hub keeps the opened document, with the source XML of unchanged
  paragraphs, so a save rewrites only what changed.
- **Refusals agents can use.** A hub can refuse a stale or forbidden change with a reason; a
  merge can't refuse anything.
- **No new core dependency.** A third-party merge engine's panic would abort the web build.

What a CRDT would give people still comes from the copies and the hub's log: a full local copy,
offline editing with automatic merge (A3), agent drafts (A3), history and attribution. The log
is one ordered history of changes, each with its author, base version and character splices,
which is the input Eg-walker-style merging needs. Peer-to-peer editing with no hub at all stays
a later option (A6), only if it is needed.

## As built

Everything below is on `main` and runs in one process: the app's window, or a headless
`wordcraft-cli`, holds the room. Networking is A3.

### Block ids (A0.2, #2; kept in Word files, A5.1, #69)

Every paragraph and table, in every story and in table cells, has a `doc::BlockId`
(`crates/doc/src/block_id.rs`). An id is random, not counted, so copies that add blocks at the
same time don't make the same ones. Editing a block keeps its id. After every command,
`Document::ensure_block_ids` gives new or copied blocks ids of their own, looking only at blocks
that aren't the same allocations as before the command. A block a command took away and made
again, holding the same, gets its id back. An id is not part of what a block holds: `==` ignores
it, the JSON form leaves it out, and the markup kept from a file doesn't depend on it.

A .docx keeps the ids (#69, `crates/docx/src/write/para_ids.rs`), so a copy saved and opened
again names its paragraphs as the room does. Every paragraph is written with a paragraph id
(`w14:paraId`, 31 bits, unique in the file): the block id's low 31 bits, or a free value when
another paragraph has those. When that can't hold the whole 64-bit id, the paragraph also carries
`wc:blockId` (16 hex digits) in WordCraft's own namespace, `urn:wordcraft:docx:2026`, which every
body part declares ignorable (ECMA-376 Part 3, Markup Compatibility); tables, which have no
paragraph id, carry `wc:blockId` only. On open, `wc:blockId` is read first, then `w14:paraId`, so
Word's own paragraph ids become block ids and are written back unchanged; repeated, zero or
malformed ids are split apart by `ensure_block_ids`. Word drops `wc:blockId` but keeps the
paragraph id, which is what the Compare step of A5 (#38) matches a Word-edited copy on.
`crates/docx/tests/it/block_ids.rs` pins it, hostile values included.

### Changes that apply to any copy (A0.3, #3; A1.1, #7)

What a command changed becomes one `doc::ChangeSet` (`crates/doc/src/change.rs`). A change
names blocks by id, never by place, so it still applies to a copy in which other blocks moved,
came or went. It holds:

| Part | What it carries |
|---|---|
| `edits` (`ParaEdit`) | A paragraph edited in place: only the characters the edit replaced, with their formatting and objects, and the paragraph's properties when they changed. Typing a letter in a 188-page document makes a change holding that letter. |
| `splices` (`Splice`) | Blocks taken away and put in, whole and with their ids, in a story or a table cell, between what both versions share at the start and end (found by address, as `Document::same_content` does). Enter, joining paragraphs, and moving or deleting blocks are splices. Each carries the window's text as a character splice (paragraph marks as U+2029, tables as U+FFFC) for mapping places. |
| `tables` (`TableShell`) | A table edited in place whose own properties changed. |
| `parts` (`PartChange`) | Headers, footers, notes, comments' text and text boxes added, removed or changed. |
| `fields` (`FieldChange`) | Document fields: comments, pictures and kept parts by key; revisions as appended; styles, lists, settings and the rest whole. |

`Document::apply` (`change/apply.rs`) checks that the copy holds what the change was made on:
the blocks it takes away must be there, in order, holding what they held, and so must the
paragraphs it edits (by fingerprint). Otherwise it returns an error and leaves the copy as it
was. A change holds at most `MAX_CHANGE_ITEMS` items, so a hostile one can't make applying it
slow.

Each session keeps its last changes in `engine::ChangeLog` (`MAX_CHANGES` = 1,024, about
`MAX_CHANGE_BYTES` = 64 MB). Agents follow them with `document.changes {since}` on the control
channel and `get_changes` in MCP ([mcp.md](mcp.md#following-changes)).

### Moving a change past another, and mapping places (A1.1, #7)

`ChangeSet::rebase` (`change/rebase.rs`) moves a change made at the same time as another so it
applies after it. Two changes **clash** when they change the same thing: the same characters of
a paragraph (putting text in inside characters the other replaces counts), the same blocks, the
properties of the same paragraph or table, the same part or document field; or when one changes
what the other takes away, or puts blocks in between blocks the other takes away. Otherwise
either order gives the same document under the same ids. Where both put text in at the same
place, `Side` says which goes first: what the hub accepted earlier.

`ChangeSet::map` (`change/map.rs`) moves a place (`Anchor`: block id and byte offset) through a
change. It answers `Mapped::At` a new anchor, or `Before`/`After` a block when the text around it
is gone. After a split, places past the split follow their text into the new paragraph.

### Command scopes (A0.4, #4)

Every command has a scope (`engine::scope`): `Document`, `Selection`, `View`, `File`, `App` or
`Ui`. A command's scope is its id prefix's (`Scope::by_prefix`) unless `OVERRIDES` gives another,
and covers everything it can do: a command that can change the document is never `Selection` or
`View`. `POSITIONS` names the params that are places in the document, which a change made
meanwhile moves. `tests/scopes.rs` checks that every command has a scope, that selection and view
commands never change the document, and that every place param is listed.

### Rooms: copies and the hub (A1.2, #9)

A shared document is a room (`engine::room`, `crates/engine/src/room.rs`):

- **`Replica`** (`room/replica.rs`) is a participant's copy: a `Session` of their own. Its
  commands change the copy at once. `send` sends each command's change on its own, made on the
  version the copy got last, with one batch in flight: it doesn't send again until what it sent
  has come back. `receive` makes the changes the hub accepted since, past the copy's own edits
  the hub hasn't accepted (which move past them, with the caret and selection,
  `Session::apply_change`). `sync` does both.
- **`Hub`** (`room/hub.rs`) holds the document at its latest version, the changes that made the
  latest versions, and who may change it. `submit_all` takes a copy's changes made on one base
  version: it moves them past the changes accepted since, accepts them up to the first that
  clashes, and sends back that one and those after it (`Took`, `Bounce::Clash`). It sends all of
  them back, leaving the document as it was, when the first clashes, when the base is too old
  (`Bounce::Version`), when one doesn't apply or reuses an id (`Bounce::Refused`), when one is
  more than the sender's role allows, when the sender is out of the room, or when the room is
  closed.
- **Making an edit again.** When a change of its own clashes, the copy drops it and the edits
  after it, and runs the same commands again on the new version, from the same places: the
  caret, selection and place params moved through the other change. Paste pastes what was
  copied then, typing keeps the formatting picked for it, and a macro makes its steps where they
  were made (`tests/room.rs` pins these). An edit whose text another participant took away is
  not made elsewhere; its sender is told, naming the command. A copy that falls behind the
  changes the hub keeps reads the document again, and gives up on an edit after `MAX_TRIES` (3)
  such reads.
- **`Room`** wraps the hub behind a lock and signals when it accepts a change
  (`Room::on_change`, `Room::wait_past`). It can say where and by whom versions changed
  (`Room::where_changed`, `Room::changes_by`), which the assistant's Show the change uses.

| Bound | Value |
|---|---|
| Changes the hub keeps for copies that are behind (`MAX_KEPT`, `MAX_KEPT_BYTES`) | 16,384, about 64 MB |
| Changes one `submit_all` takes (`MAX_BATCH`) | 1,024 |
| Participants and invites (`MAX_PARTICIPANTS`, `MAX_INVITES`) | 4,096 each |
| Characters in a participant's name | 128 |

Every copy makes the accepted changes in the hub's order, so all end up holding the same
document under the same block ids. A property test (`copies_editing_at_once_end_up_holding_the_same_document`)
and a threaded test (`copies_on_threads_end_up_holding_the_same_document`) check it.

### Roles and keys (A1.3, #12)

Each participant has a `room::Role`. Each role may do what the ones before it may, and more:

| Role | May |
|---|---|
| Viewer | read, and move their own caret, selection and view |
| Commenter | also add comments and replies, edit and delete their own, and resolve discussions |
| Suggester | also edit, always as tracked changes in their name (Track Changes is on for them alone), and withdraw their own |
| Editor | also edit directly, and accept and reject tracked changes |
| Owner | also change the application's settings in their copy |

The room's own process joins people and agents under a name and a role (`Room::join`). Anyone
else joins with a key the room handed out for one name and role (`Room::invite`,
`Room::join_with`), so they can't claim another's name. A name is someone's in the room for good,
compared without case, since their comments and tracked changes keep it. Revoking a key
(`Room::revoke`, by key or by the name it was for) takes its participants out: the hub refuses
their changes.

An invite can also limit itself (#74): `expiresIn` stops the key working after a while (a minute
to 30 days), as if taken back; `uses` lets it join a set number of times. A **remote** invite, for
an agent on another machine, is never owner and can't be made one, reaches no file unless it
names some, lasts a day unless it says otherwise, and gets its exports back as bytes in the reply.
Only an invite's key ever leaves the computer: the window's key acts as the person.

The owner can change an invited participant's role later (`room.role`, `Hub::set_role`): the
invite takes the new role, so a copy that joins with its key later has it, and the hub holds the
participant's next change to it; their copies take it with the next changes they receive. The
window keeps the role it joined with, and the built-in assistant's role is chosen in its pane.
`room.participants` lists the invites not taken back (never their keys), with how many copies
joined with each and when it expires.

Roles are held twice (`room/rules.rs`). The copy undoes a command that changes more than the
role allows (`Session::role`), and holds Repeat, a macro, Undo and Redo to the role as a whole.
The hub checks each change again. A commenter's change must leave the document as it was but
for their own comments; a suggester's must do the same once every tracked change is rejected,
with others' tracked changes where they were. Both checks compare only the blocks and parts the
change touched, so a check takes as long as the change, whatever the document's length.

A copy runs only commands whose scope its role allows (`Role::allows`): document, selection and
view for everyone, app settings for the owner, files and windows for no copy (the shared
document is the room's). Exports (`scope::EXPORTS`) are every role's.

### File › Share (A1.5, #33, #70)

File › Share is a page of the backstage after Print (keytip Alt, F, Z), which the Share button
on the tab strip opens too (`crates/ui-egui/src/share_page.rs`). It has Share this document and
Stop sharing; Invite, with a name, a role (each explained) and, for an agent, which files it may
use, never more than File › Options › Agents allows; the new key shown once, with the
`WORDCRAFT_CONTROL_KEY=… wordcraft-cli mcp --connect …` line an MCP client runs; and the list of
who is in the document and who is invited, each with their role, files and whether they are
connected, a role to change and Remove access. Every control runs a command (`room.share`,
`room.invite`, `room.role`, `room.revoke`, `room.stop`; the six `room.*` commands are registered,
scoped to the window and refused by a session without one), the same the control channel and
MCP run, so a program can do whatever the page does; the page only reads the room's state.

### The window's room and agents' copies (A1.3, #13 and #45)

The app's window shares the document it shows (`Room::host`, control method `room.share`). Its
own session is the owner's copy, lent to the `Replica` for each command and each frame
(`Replica::lend`, `crates/ui-egui/src/share.rs`), so the person's typing goes through the hub like
anyone's and comes back made again, not lost, when it clashes with an agent's. The hub wakes the
window when it accepts a change, and every frame brings in what others changed. The window keeps
the shared document up to date while it shows another one, and leaves the input method alone
while it does.

An agent connects to the control port with an invite's key (`room.invite`). The control server
finds the room in the `Lobby` it shares with the window and gives the connection a copy of its
own, which answers on the connection's thread (`room::hosted`). The agent's commands never wait
for the window's frame, nor the window's for an agent. `engine.execute` returns once the hub took
all of the command's changes, in batches for a long macro; when others' changes keep sending them
back (`MAX_SENDS` = 4 times in a row), they stay in the copy and go first with the next request.
`wordcraft-cli mcp --connect` with the invite's key gives an agent its copy through MCP's tools.
Revoking the invite or stopping sharing ends the agent's connection. The methods are listed in
[control-protocol.md](control-protocol.md#shared-documents).

### Files agents can use (#31, #64)

Which files the commands of participants who joined with a key can read and write is the
person's setting, File › Options › Agents (`engine::agent_files`): none (the default), only some
folders, each read-only or read and write, or any file. The setting is the most any agent gets;
an invite's `files` can only give less. The reach is computed for each request and withdrawn
mid-request when it narrows, an invite is revoked, or the room closes. The app's settings folder,
which holds control keys, is never in reach. Copies may export to new files only, and never
save the shared document, since a save stamps it.

### Undo per participant (A1.4, #14)

Undo stays each participant's own. Undo steps are snapshots of the whole document, so a copy's
newest undo and redo steps keep the changes others made since (`Session::apply_theirs`, up to
1,024 of them). Undo makes the step's change undone in the document as it is now, moved past
theirs as the hub moves an edit, and leaves theirs in. When another participant changed what the
step changed (the same characters, blocks, part or document field), the step stays and Undo names
who changed it. What the participant types after another's change is a step of its own. A copy
that makes its edits again after a clash puts back the steps it had before them.
`tests/room_undo.rs` pins this.

### The built-in assistant: a co-editor in the pane (#15)

The Assistant pane on the right of the window is a conversation with an AI assistant that edits
the document **while the person keeps editing it**. It is a participant of the room like any
other (`crates/assist`, `crates/ui-egui/src/assist.rs`), so everything above holds for it: its
own copy, its changes ordered by the hub, its role held by its copy and by the hub, its own undo.
The person never waits for it, and it never waits for the person's frame. The pane, its backends
and its settings are in [assistant.md](assistant.md); this section is how it co-edits.

**Joining and leaving.** The first prompt in the pane shares the document if it isn't shared
yet (`WordApp::share_document`), then starts a conversation (`Assistant::start`). The
conversation takes an invite of its own from the room, under its backend's name: "ChatGPT",
"Claude Code", "Codex", "Pi"…, or "ChatGPT (2)" when the room already had a "ChatGPT"
(`Room::invite_numbered`). That name is the author of its comments and tracked changes. When the
conversation ends (a new conversation, another role, Use this document, closing the window), the
invite is taken back (`Room::revoke`): its copy is out of the room and an agent's key opens
nothing from then on. One document is shared at a time; if another open document is shared, the
pane offers **Use this document** (`assist.useDocument`) instead of starting.

**Roles in the pane.** The pane's Role box picks the conversation's role, and says what it means
(`role_note`): Viewer ("It reads the document and answers here; it changes nothing"), Commenter,
Suggester ("Its edits are tracked changes in its name, for you to accept or reject") or Editor
("Its edits won't be tracked: it changes the document directly"). Left alone, a conversation
takes File › Options › Assistant's setting (`assistant.defaultRole`), suggester unless the person
chose another. A prompt for another role starts a new conversation with a new invite, since a
participant's role is fixed by its invite. Owner is never offered: an assistant can't change
WordCraft's settings, and a setting of `owner` is read as suggester.

**Where its copy runs.** Two kinds of backend, one room:

- **ChatGPT** runs its model loop in WordCraft (OpenAI's Responses API, with the person's ChatGPT
  sign-in). Its tool calls (WordCraft's MCP tools, `wordcraft_mcp::tools`) run on a copy the
  conversation holds on its own thread (`CopyTools`, a `room::hosted` copy).
- **ACP agents** (Claude Code, Codex, Pi, a custom agent) are processes WordCraft starts and talks
  to over the Agent Client Protocol. The agent runs its own model loop and reaches its copy
  through WordCraft's MCP server (`wordcraft-cli mcp --connect`) on the room's own port with the
  conversation's key. That port takes only invites' keys; the agent never sees the window's.

Either way the assistant can do only what its role and the command scopes allow, and reach only
the files File › Options › Agents lets agents use.

**A turn, while the person types.** Each turn starts with the assistant's copy selecting what
the person selected in the window, found by block id and offset, so edits the copy hasn't had yet
don't move it (`Assistant::ask_at`), and each of its tool calls first brings in what others
changed since (`room::hosted`). The prompt carries the selection, the
paragraphs around it and the outline as text, within fixed caps (`assist::Context`). The
assistant's edits change its copy at once and go to the hub as they are made. Meanwhile the
person types, and other agents edit; the hub orders all of it. When the assistant and the person
change the same characters, whichever the hub took second is made again after the other, from
the same places, so neither's edit is lost unless the other took its text away. The window draws
the assistant's changes as they arrive, between frames.

**Tracked changes in its name.** As a suggester, every edit the assistant makes is a tracked
change by its name, whatever the document's Track Changes setting; the hub refuses any change
from it that isn't. The pane's **Accept all** and **Reject all** act on its changes only
(`review.acceptAll`/`review.rejectAll` with `author`), in the body, headers, notes and text
boxes, and **Review changes one by one** opens the Review tab. A tool call's row in the pane links
to exactly what that call changed (**Show the change**, `assist.showChange`, from the room's
record of who changed which versions, `Room::where_changed`). As an editor, its edits are direct
and untracked. Either way, Undo in the window takes back only the person's own steps, never the
assistant's.

**Stopping.** Stop ends a turn at once; an agent that doesn't stop within 10 seconds is ended and
starts again on the next turn. Edits the assistant made before Stop stay, as tracked changes in
its name when it is a suggester. The window's frames never wait for any of this.

**Several assistants at once.** The pane shows one conversation, but the room takes any number of
participants: a second assistant (another backend, or the same one as "ChatGPT (2)") or outside
agents can edit at the same time, each with a name, a role and a copy of its own.
`apps/wordcraft-cli/tests/it/gate.rs` has a person, ChatGPT and an ACP agent edit one document at
once and checks that every copy ends with the hub's document, each assistant's edits are tracked
in its name, the person's Undo takes back only the person's typing, and stopping or killing one
assistant leaves the other going. `the_window_never_waits_for_its_assistants`
(`crates/ui-egui`) times the real window's frames while two assistants hold their turns.

**Desktop only.** The web app has the pane and the commands but no backend: ChatGPT's sign-in
needs a loopback port and ACP agents are programs a web page can't start.

### Presence (A1.5, #32, #71, #72)

Everyone sees where everyone else is. Presence sends no document text, only places.

- **Places as anchors.** Each copy tells the hub where its participant's selection is
  (`room::Presence`: where it starts and the caret, each an anchor, a paragraph's id and a byte
  offset in its text), as of the version the copy has (`Replica::present`, `Hub::set_presence`).
  The window tells the person's place while it shows the shared document; an agent's place is
  where its last command that moved its caret or edited left it. The hub keeps at most 64 places
  (`MAX_PRESENCES`): past them, the one told longest ago goes.
- **Moved with the text.** A reader gets every place moved through the changes since it was told
  and through its own edits not yet back (`Presence::through`, `ChangeSet::map`), so a place
  stays on the words it was on while others type. A participant's own typing goes past their own
  caret (`ChangeSet::map_past`), as in their copy. A place in a part that is gone is dropped.
- **Colours.** Each participant has one of eight colours (`COLORS`), the same for every copy with
  their name, and theirs again when they come back unless someone else has it by then. The
  window draws them from `Tokens::participant_colors`, the same in both themes, each readable
  with white text.
- **For agents.** `room.participants` gives each participant `color` and `selection` (anchors
  as `select.range` takes them, with the story and the top-level block, or `null` while
  unknown), so an agent can keep clear of where a person types.
- **In the window.** While it shows the shared document, the window draws each other
  participant's selection shaded and caret in their colour, with a flag carrying their name for
  three seconds after they move, while the pointer is over their caret, and always for the one
  the view follows (`canvas/presence.rs`). Beside Share, a row of avatars drawn in code
  (initials in their colour; past four, a "+n" menu) says who else is there. Clicking one follows
  that participant's caret until the person scrolls, clicks the page, types (an input method
  too), pastes or cuts; `ui.follow` does the same for scripts.
- **Cost.** The window reads the others' places only when the room's presence stamp changed or
  its own edits aren't back yet, so an idle room costs nothing. Others' selections are laid out
  only on the pages on screen (and one either side), and there only the lines within a screen's
  height of what is seen, a turned cell's text too. What of each story is near the screen is kept
  between frames until the layout or the view changes, so others moving costs no walk of a long
  page (Web Layout). At most 64 others and 2,000 selection rectangles are drawn: eight
  participants each selecting a 20,000-line document cost about 0.1 ms a frame.

## What it guarantees, and what pins it

| Guarantee | Where it is checked |
|---|---|
| A change lands only on the content it was made on | `Document::apply`; `crates/doc/src/change/tests.rs` |
| A second copy applying every change stays identical to the first | `crates/engine/src/tests/changes.rs` (every command, and a property test of random edits) |
| Copies editing at once end up with the hub's document, ids included | `crates/engine/src/tests/room.rs` (property test, threads) |
| An edit that clashes is made again from the same places, not lost | `crates/engine/src/tests/room.rs` |
| Roles hold in the copy and at the hub | `crates/engine/src/tests/room_roles.rs` |
| Agents' copies, keys and revocation | `crates/engine/src/tests/room_host.rs`; `crates/ui-egui/src/share.rs` tests |
| Invites that expire, join a set number of times or are remote; `room.role` | `crates/engine/src/tests/room_remote.rs`, `crates/engine/src/tests/room_host.rs` |
| File › Share runs the room's commands and shows its state | `crates/ui-egui/src/share_page/tests.rs` |
| Agents reach only the files the setting allows | `crates/engine/src/tests/room_files.rs` |
| Undo takes back only one's own steps | `crates/engine/src/tests/room_undo.rs` |
| Places follow the text and reach every copy, with a colour each | `crates/engine/src/tests/room_presence.rs` |
| The window draws others where they are, follows them, and stays in time with long selections | `crates/ui-egui/src/canvas/presence.rs` tests |
| A person, ChatGPT and an ACP agent edit at once (fakes) | `apps/wordcraft-cli/tests/it/gate.rs` |
| No command panics on junk params | `crates/engine/src/tests.rs::hostile_params_never_panic` |

## Known limits today

- **One process.** The room lives in the app or a headless `wordcraft-cli`; there is no network
  hub, durable log or offline editing yet (A3). Remote invites (#74) set the rules for agents on
  other machines, but the way in for them, MCP over HTTP (#75) through an outbound relay (#76),
  isn't built: until then only a developer's SSH tunnel reaches the room's port.
- **Tracked edits at the same moment take turns.** Any two changes that both change the
  document's tracked-change list (`Document.revisions`) clash, so two participants' tracked
  edits made at the same moment go one after the other: the second is made again after the
  first.
- **Undo around others' edits.** Undo refuses a step whose content someone else changed since,
  and a tracked step can't be undone after anyone else's tracked change. Text-level undo is A4.
- **No agent API yet.** Agents use the same commands as people, addressed by places; guarded,
  content-addressed edits, leases and events are A2.
- **The assistant doesn't see where others are.** The window draws the assistant's caret,
  selection and name like any participant's (#32), but the assistant has no tool for
  `room.participants` yet, so it learns of the person's typing only from the changes, not from
  where the person is.
- **The assistant doesn't wake on its own.** It works when the person asks in the pane (or a
  script asks through `assist.*`); a comment that mentions it doesn't reach it until A2.

## What comes next

Each milestone's gate is a step of the scenario below, run as an automated test with scripted
agents. Issues: #28 (parent) and one sub-issue per milestone.

| Milestone | Scope | Gate |
|---|---|---|
| **A0 Foundations** (done) | Block ids (#2), changes that apply to any copy (#3), command scopes (#4), the hub spike (#5, kept open as a reference, never merged), this design doc (#40) | A second copy follows a person's edits exactly |
| **A1 Shared instance** (#29) | In one process: in-app hub, agents' copies on their own threads, keys and roles, undo per participant (done: #7, #9, #12, #13, #14, #45, #64, File › Share #33 in #70, and presence #32 in #71 and #72); left: the gate test (#34) | Two agents and a person edit at once, with no stalls |
| **A2 Agent API** (#35) | On one machine: content-addressed reads, guarded edits, rewrites as word-level tracked changes, room chat, leases (Review › Block Authors), people first (an agent's direct edit to a paragraph a person just typed in is refused as busy), MCP Events push and poll | Scenario steps 1 to 4 pass with scripted agents |
| **A3 Network rooms** (#36) | Agents in remote sandboxes join the person's desktop room (design #73): remote-safe invites (done, #74), MCP over Streamable HTTP (#75), an outbound relay so no tunnel is needed (#76). Then the hub over TLS with keys, durable write-ahead log, snapshots that keep source XML, app and browser copies, offline editing in Merge and Suggest modes, version history, agent drafts (fork and merge), signed webhooks | With the hub killed or a laptop offline for an hour, nothing is lost |
| **A4 Hardening** (#37) | Seeded simulation over a lossy network, protocol fuzzing, text-level undo, 10 people on 188 pages | Simulation clean, latency targets met |
| **A5 Interop** (#38) | Block ids saved as Word's paragraph ids (`w14:paraId`, done in #69); edits made in Word merged back through Compare as tracked changes, and names and dates through Word | The scenario's output round-trips through Word |
| **A6 Peer-to-peer** (#39), only if needed | Copies merge each other's logs with no hub | Two laptops edit offline and converge |

### The scenario

1. Minoru opens report.docx and shares it. A copy editor agent (suggester), a fact checker
   agent (commenter) and a formatter agent (editor) join over MCP.
2. Minoru types in section 1 while the copy editor proposes edits in section 3, the fact checker
   comments on section 2 and the formatter restyles headings. No caret jumps, and every change
   carries its author.
3. In a Word comment, Minoru writes "@Copy editor tighten this". The mention wakes the agent
   through MCP Events. It leases the introduction, waits while Minoru is typing there, then
   proposes word-level tracked changes and replies with a one-line rationale.
4. Minoru accepts some changes, rejects others, and reverts the formatter's work with one
   command. Undo takes back only Minoru's own steps.
5. Minoru's laptop goes offline for an hour while the agents keep working. On reconnecting, his
   edits merge on their own, even in the sentence he and the copy editor both changed. In
   Suggest mode, his edits to that sentence come back as his suggestions. Nothing is lost.
6. The hub is killed and restarted mid-session. No acknowledged edit is lost, and the saved
   .docx re-opens with every tracked change and comment attributed.

### What comes next for the assistant

The assistant gains with each milestone, as every agent does:

- **A1.5 presence (#32, done):** its caret, selection and name are drawn in the text in its own
  colour while it works. Next, a tool for the person's place, so it can keep away from where the
  person is typing.
- **A1.6 gate (#34):** two agents and a person edit at once with no stalls, the pane's assistant
  among them.
- **A2 (#35):** guarded, content-addressed edits, so a stale rewrite is refused with the current
  text instead of landing; rewrites as word-level tracked changes with a rationale; room chat;
  leases on a range; a person's paragraph kept free while they type in it; mentions in comments
  ("@ChatGPT tighten this") waking the assistant through MCP Events.
- **A3 (#36):** the assistant works in a draft copy and merges it back, and follows the room over
  the network like any participant.

### Events for agents (A2, A3)

Agents follow the room through MCP Events (draft SEP-3415) instead of polling: `mentioned`,
`assigned`, `blocks_changed`, `suggestion_resolved`, `block_free` and `saved`. Delivery is push
on the open MCP session and poll (`wait`) in A2, and signed webhooks from the hub in A3. The
cursor is the room version, so an agent whose subscription lapsed replays what it missed from the
log. A subscription is bound to the participant's key and role and ends when either is taken
away; lifetimes are finite. Payloads carry no document text. Agents never get events caused by
their own changes, and mentions between agents stop after three hops. The protocol mapping lives
in one module of `mcp` behind an experimental flag, since the spec is still a draft.

## Decisions

| Date | Decision | By |
|---|---|---|
| 2026-11-05 | One authoritative hub per document, as long as editing feels as seamless as Google Docs and Notion; the CRDT-like features (local copies, offline, automatic merge, drafts, history, attribution) are still delivered, and a CRDT stays possible (A6) | Minoru |
| 2026-11-05 | Every participant edits a copy of its own and sends results (changes), not commands; the earlier design of one session with each participant's state swapped in is dropped | plan |
| 2026-11-05 | Overlapping offline edits: both modes, as a room setting each person can change for their own copy. Merge (the default) keeps both people's edits; Suggest turns those that land in text someone else changed into tracked changes | Minoru |
| 2026-11-05 | Agents join as suggesters by default; the owner can invite them as editors | Minoru |
| 2026-11-05 | Agents follow the room through MCP Events (push, poll with `wait`, webhooks) | Minoru |
| 2026-11-05 | The hub checks the characters an edit touched inside a paragraph, not the whole paragraph (from the hub spike) | plan |
| 2026-11-07 | Which files agents can use is the person's setting, the most any agent gets; an invite can only narrow it, so None stops every agent (#31) | Minoru |

### What the hub spike found (A0.5, #5)

The spike ran a hub and three copies of a 188-page document. Checked per paragraph, the accept
rule held between paragraphs but not inside one: at 200 ms from the hub, the second of two
writers in one paragraph got through only once the first stopped typing (31 s median). No bounce
was a real conflict; the writers never touched the same characters. Hence the per-character
check of A1.1. Applying a change took 0.2–0.6 ms in a copy and 0.3 ms at the hub, and laying the
pages out again 14–16 ms, as after one's own keystroke. Every copy ended identical to the hub,
ids included.

## Code map

| Piece | Where |
|---|---|
| Block ids | `crates/doc/src/block_id.rs` |
| Changes, applying, rebasing, mapping | `crates/doc/src/change.rs`, `change/{apply,chars,rebase,map}.rs` |
| A session's change log | `crates/engine/src/session/changes.rs` |
| Others' changes in a session; undo past them | `crates/engine/src/session/shared.rs` |
| Command scopes | `crates/engine/src/scope.rs` |
| Rooms, roles, lobby | `crates/engine/src/room.rs` |
| Hub, copy, role rules, hosted copies | `crates/engine/src/room/{hub,replica,rules,hosted}.rs` |
| Files agents can use | `crates/engine/src/agent_files.rs` |
| The window's room; File › Share | `crates/ui-egui/src/share.rs`, `crates/ui-egui/src/share_page.rs` |
| Places (presence) | `crates/engine/src/room.rs` (`Presence`), `room/{hub,replica}.rs` |
| Others in the window, avatars, following | `crates/ui-egui/src/canvas/presence.rs`, `crates/ui-egui/src/avatars.rs` |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-07 | minor | Presence as built (#32, #71, #72) replaces the placeholder: places as anchors told as of a version and moved with the text, eight colours, `room.participants` for agents, others' carets, selections and names in the window, avatars and following, and its cost; two guarantee rows, two code map rows; "the assistant's place isn't drawn" becomes "the assistant doesn't see where others are"; A1 row and the assistant's A1.5 line updated |
| 2026-11-07 | minor | Block ids kept in Word files (#38, #69): paragraphs carry `w14:paraId` and, when it can't hold the whole id, an ignorable `wc:blockId`; tables `wc:blockId`; read back on open; the A5 row says the first step is done |
| 2026-11-07 | minor | File › Share (#33, #70) and remote-safe invites (#74) as built: the page, `room.role`, revoking by name, invites that expire, join a set number of times or are remote; "No File › Share yet" dropped from known limits; A1 and A3 rows updated |
| 2026-11-07 | minor | The built-in assistant as a co-editor: a "Who edits together" table (the person, the pane's assistant, outside agents), and how the pane's assistant joins and leaves, its roles, where its copy runs, a turn while the person types, tracked changes in its name, Stop, several assistants at once, its known limits and what comes next for it (Minoru's review of #68) |
| 2026-11-07 | major | First version (#40): the design as built on `main` (block ids, changes, rebasing and mapping, scopes, rooms, roles and keys, agents' copies, the agents' file setting, undo per participant, the assistant), presence as a placeholder (#32), known limits, milestones A0–A6, the scenario, events and the decisions |
