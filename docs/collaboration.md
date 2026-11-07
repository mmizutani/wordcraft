# Co-editing: people and agents edit one document at once

> **Last reviewed:** 2026-11-07 · **Last updated:** 2026-11-07 · **Change:** major (first version: the design as built on `main` through A1.4 and the agents' file setting, what comes next by milestone, and the decisions; presence is a placeholder until A1.5 lands, #40) · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

This is the design of co-editing in WordCraft: several people and AI agents edit one document at
the same time, each change carries its author, and nothing is lost or corrupted. It is the
reference the co-editing pull requests cite. It describes what is built on `main`, what comes
next, and the decisions behind both. Work is tracked in issue #28 and its sub-issues; the gap is
#22 in [gaps.md](gaps.md).

Related: [architecture.md](architecture.md) (data flow), [control-protocol.md](control-protocol.md#shared-documents)
and [mcp.md](mcp.md#shared-documents) (how agents join), [assistant.md](assistant.md) (the
built-in assistant, which joins as a participant).

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

### Block ids (A0.2, #2)

Every paragraph and table, in every story and in table cells, has a `doc::BlockId`
(`crates/doc/src/block_id.rs`). An id is random, not counted, so copies that add blocks at the
same time don't make the same ones. Editing a block keeps its id. After every command,
`Document::ensure_block_ids` gives new or copied blocks ids of their own, looking only at blocks
that aren't the same allocations as before the command. A block a command took away and made
again, holding the same, gets its id back. An id is not part of what a block holds: `==` ignores
it, files don't store it, and the markup kept from a file doesn't depend on it.

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
(`Room::revoke`) takes its participants out: the hub refuses their changes.

Roles are held twice (`room/rules.rs`). The copy undoes a command that changes more than the
role allows (`Session::role`), and holds Repeat, a macro, Undo and Redo to the role as a whole.
The hub checks each change again. A commenter's change must leave the document as it was but
for their own comments; a suggester's must do the same once every tracked change is rejected,
with others' tracked changes where they were. Both checks compare only the blocks and parts the
change touched, so a check takes as long as the change, whatever the document's length.

A copy runs only commands whose scope its role allows (`Role::allows`): document, selection and
view for everyone, app settings for the owner, files and windows for no copy (the shared
document is the room's). Exports (`scope::EXPORTS`) are every role's.

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

### The built-in assistant (#15)

The assistant is one more participant. A conversation takes an invite of its own from the
window's room, under its backend's name and the role from `assistant.defaultRole` (suggester
unless the person chose another; never owner), and works on a hosted copy, so scopes, roles and
the file setting hold for it as for any agent. See [assistant.md](assistant.md).

### Presence (A1.5, #32): in progress

_Placeholder: presence is being built (#32). This section will describe it once it lands on
`main`._ The plan: each copy tells the room where its caret and selection are, as anchors; the
hub keeps each participant's and moves them through every accepted change (`ChangeSet::map`);
the window draws others' carets, names and selections in each participant's colour from
`theme::Tokens`, with a row of avatars drawn in code and following someone's caret; agents read
where everyone is from `room.participants`. Presence sends no document text.

## What it guarantees, and what pins it

| Guarantee | Where it is checked |
|---|---|
| A change lands only on the content it was made on | `Document::apply`; `crates/doc/src/change/tests.rs` |
| A second copy applying every change stays identical to the first | `crates/engine/src/tests/changes.rs` (every command, and a property test of random edits) |
| Copies editing at once end up with the hub's document, ids included | `crates/engine/src/tests/room.rs` (property test, threads) |
| An edit that clashes is made again from the same places, not lost | `crates/engine/src/tests/room.rs` |
| Roles hold in the copy and at the hub | `crates/engine/src/tests/room_roles.rs` |
| Agents' copies, keys and revocation | `crates/engine/src/tests/room_host.rs`; `crates/ui-egui/src/share.rs` tests |
| Agents reach only the files the setting allows | `crates/engine/src/tests/room_files.rs` |
| Undo takes back only one's own steps | `crates/engine/src/tests/room_undo.rs` |
| A person, ChatGPT and an ACP agent edit at once (fakes) | `apps/wordcraft-cli/tests/it/gate.rs` |
| No command panics on junk params | `crates/engine/src/tests.rs::hostile_params_never_panic` |

## Known limits today

- **One process.** The room lives in the app or a headless `wordcraft-cli`; there is no network
  hub, durable log or offline editing yet (A3).
- **Tracked edits at the same moment take turns.** Any two changes that both change the
  document's tracked-change list (`Document.revisions`) clash, so two participants' tracked
  edits made at the same moment go one after the other: the second is made again after the
  first.
- **Undo around others' edits.** Undo refuses a step whose content someone else changed since,
  and a tracked step can't be undone after anyone else's tracked change. Text-level undo is A4.
- **No agent API yet.** Agents use the same commands as people, addressed by places; guarded,
  content-addressed edits, leases and events are A2.
- **No File › Share yet.** Sharing starts from the control channel or MCP (`room.share`); the
  app's menu comes with #33.

## What comes next

Each milestone's gate is a step of the scenario below, run as an automated test with scripted
agents. Issues: #28 (parent) and one sub-issue per milestone.

| Milestone | Scope | Gate |
|---|---|---|
| **A0 Foundations** (done) | Block ids (#2), changes that apply to any copy (#3), command scopes (#4), the hub spike (#5, kept open as a reference, never merged), this design doc (#40) | A second copy follows a person's edits exactly |
| **A1 Shared instance** (#29) | In one process: in-app hub, agents' copies on their own threads, keys and roles, undo per participant (done: #7, #9, #12, #13, #14, #45, #64); left: presence (#32), File › Share (#33), the gate test (#34) | Two agents and a person edit at once, with no stalls |
| **A2 Agent API** (#35) | On one machine: content-addressed reads, guarded edits, rewrites as word-level tracked changes, room chat, leases (Review › Block Authors), people first (an agent's direct edit to a paragraph a person just typed in is refused as busy), MCP Events push and poll | Scenario steps 1 to 4 pass with scripted agents |
| **A3 Network rooms** (#36) | Hub over TLS with keys, durable write-ahead log, snapshots that keep source XML, app and browser copies, offline editing in Merge and Suggest modes, version history, agent drafts (fork and merge), signed webhooks | With the hub killed or a laptop offline for an hour, nothing is lost |
| **A4 Hardening** (#37) | Seeded simulation over a lossy network, protocol fuzzing, text-level undo, 10 people on 188 pages | Simulation clean, latency targets met |
| **A5 Interop** (#38) | Block ids saved as Word's paragraph ids (`w14:paraId`); edits made in Word merged back through Compare as tracked changes | The scenario's output round-trips through Word |
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
| The window's room | `crates/ui-egui/src/share.rs` |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-07 | major | First version (#40): the design as built on `main` (block ids, changes, rebasing and mapping, scopes, rooms, roles and keys, agents' copies, the agents' file setting, undo per participant, the assistant), presence as a placeholder (#32), known limits, milestones A0–A6, the scenario, events and the decisions |
