# WordCraft architecture

> **Last reviewed:** 2026-10-28 · **Last updated:** 2026-11-07 · **Change:** trivial (data flow: each participant's caret and selection reach the others as anchors, `room::Presence`, told as of the copy's version and moved to the reader's when read); previously trivial (data flow: the assistant's backends, ChatGPT in WordCraft over HTTPS and ACP agents as processes reaching their copies through the room's port, desktop only, and the gate test of a person and both editing at once; `assist`'s row re-counted); previously trivial (`assist`, the built-in writing assistant, at L5: a conversation joins the window's room as a participant with an invite and a role, suggester by default, and calls the MCP server's tools on a copy of its own); previously trivial (data flow: Undo in a shared document takes the participant's own step back past others' changes, or names who changed the same thing); previously trivial (data flow: the window shares its document through a room, and agents with an invite's key get copies of their own on their connections' threads, `room::hosted`); previously trivial (data flow: a participant's Repeat, macros, Undo and Redo are held to their role as a whole; names in a room are its participants' own; a comment's text is its own; others' tracked changes stay where they were); previously trivial (data flow: participants in a room have roles, `room::Role`, which their copy and the hub hold each change to); previously trivial (data flow: shared documents are rooms, `engine::room`: participants edit copies of their own, and the room's hub puts their changes in one order); previously trivial (data flow: a paragraph edited in place sends only the characters the edit replaced, and a change made at the same time as another is moved past it, `ChangeSet::rebase`); previously trivial (data flow: every command has a scope, `engine::scope`, and names the params that are places); previously trivial (data flow: what a command changed becomes one change another copy applies, `doc::ChangeSet`, and agents follow, `document.changes`); previously trivial (data flow: every paragraph and table has an id, `doc::BlockId`); previously trivial (craft-proof's row says what a build without the Language data keeps: English spelling from an installed Hunspell English or the built-in list, rule hyphenation for Bulgarian, Turkish and Russian; previously minor: proofing moved out: `wordcraft-proof` is craft-libs' `craft-proof`, its Language data craft-i18n's `language-data/` embedded from `CRAFT_I18N_DIR`; previously major: crate and layer inventory, source counts, proofing languages and module boundaries re-measured from this checkout; previously minor: `proof` is per language: English and Polish; previously major: first committed version; `plan/architecture.md` is local-only) · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

How WordCraft is built today. The longer design notes in
`plan/architecture.md` are gitignored and local to the original author's machine; this file is the
committed, current summary. Rules for working in the code are in [`AGENTS.md`](../AGENTS.md).

## Layers

On 2026-10-28, `find crates apps xtask -type f -name '*.rs' -print0 | xargs -0 wc -l` counted
333,448 Rust source lines across 16 crates, 3 apps and `xtask`. `Cargo.toml` declares workspace
members with `crates/*`, `apps/*` and `xtask`; `cargo xtask layers` verified 20 workspace packages
(16 crates, 3 apps and `xtask`) and no layering violations. The layer and line counts below are
from that command and per-directory `find ... -name '*.rs' ... | wc -l` counts on that date. Each
crate name links to its source tree.

| Layer | Crate | Lines | Job |
|---|---|---|---|
| L0 standalone | [`control-key`](../crates/control-key/src) | 87 | Per-window control-port key files shared by the app and MCP bridge |
| L0 | [`geom`](../crates/geom/src) | 1,271 | Units (points, twips, EMU, inches, cm), rectangles and measurement parsing |
| L1 | [`doc`](../crates/doc/src) | 23,719 | Document model: stories, paragraphs, runs, tables, sections, styles, numbering, fields, notes, comments, revisions and equations; edit primitives and bidi |
| L1 | [`fonts`](../crates/fonts/src) | 7,088 | Font database, metrics, outlines, HarfRust shaping, Word font substitution and OpenType MATH |
| L1 | [`metafile`](../crates/metafile/src) | 3,614 | Shared Windows Metafile parsing and drawing primitives used by format readers and renderers |
| L1 (external) | [`craft-proof`](https://github.com/storytold/craft-libs/tree/main/crates/proof) (package alias `wordcraft-proof`) | — | Proofing moved to craft-libs (CP1, 2026-11): fifteen spelling dictionaries: English (US, GB, CA), Polish, Portuguese, Italian, Croatian, Turkish, Russian, Czech, Indonesian and Spanish (Spain, Mexico, Latin America, Uruguay); language modules provide spelling, suggestions and grammar, while Bulgarian and German have hyphenation only. Its word lists and patterns are craft-i18n's `language-data/`, embedded from `CRAFT_I18N_DIR` at the commit in `craft-i18n.rev` (CI and releases set `CRAFT_I18N_REQUIRED=1`); without them English spelling falls back to an installed Hunspell English or craft-proof's built-in Moby list, and pattern hyphenation is off (Bulgarian, Turkish and Russian hyphenate by rule) |
| L2 | [`docbin`](../crates/docbin/src) | 8,815 | Word 97–2003 `.doc` reader (FIB, piece table, FKPs, SPRMs, stylesheet, lists, tables, notes, pictures) over a CFB container |
| L2 | [`docx`](../crates/docx/src) | 37,376 | OOXML WordprocessingML reader/writer (Transitional and Strict), OMML, custom properties and preserved VBA parts |
| L2 | [`formats`](../crates/formats/src) | 23,058 | TXT, Markdown, HTML, RTF, ODT and LaTeX import/export through the shared document model |
| L2 | [`layout`](../crates/layout/src) | 35,990 | Paragraph line layout (`para`), page/section flow and table layout, then display-list generation (`display`); hit testing maps points to document positions. `para` owns shaping, breaks, tabs and line geometry; page flow composes paragraphs, tables, headers/footers, notes and floats. |
| L3 | [`pdf`](../crates/pdf/src) | 4,513 | krilla PDF export: font subsets, selectable text, links, outline and tagging |
| L3 | [`render`](../crates/render/src) | 4,499 | vello_cpu rasterizer: pages and thumbnails from the display list; PNG/JPEG |
| L4 | [`engine`](../crates/engine/src) | 72,851 | `Session` and editing state; command registry in `cmd/`; file dispatch in `io`; shared documents in `room`; catalog, keymap, recovery, ribbon model, speech and view state |
| L5 | [`mcp`](../crates/mcp/src) | 1,822 | MCP server exposing commands as tools; headless or bridged to the running app |
| L5 | [`assist`](../crates/assist/src) | 12,571 | Built-in writing assistant (2026-11-05): a conversation (`Assistant`) on a thread of its own, joined to the window's room under its backend's name and a role (suggester by default, `assistant.defaultRole`); backends (`Backend`, scripted `Fake`); the MCP server's tools on its copy; capped context and transcript. The core has no network or processes, so it builds for the web; the backends that have them, ChatGPT (`chatgpt`: Sign in with ChatGPT and the Responses API over HTTPS) and ACP agents (`acp`: agent processes over stdio, and the room's port they reach their copy through), are desktop only. It may use `mcp` (same layer, one way) |
| L5 | [`zotero`](../crates/zotero/src) | 2,372 | Zotero word-processor integration protocol answered against a `Session` |
| L6 | [`ui-egui`](../crates/ui-egui/src) | 81,759 | Front end: ribbon, keytips, mini-toolbar, Backstage, canvas, rulers, panes, dialogs, i18n, themes, control channel and headless `ui_shot` example |
| apps | [`wordcraft`](../apps/wordcraft/src) | 3,740 | Desktop app (eframe + wgpu; logging, window geometry and file associations) |
| apps | [`wordcraft-cli`](../apps/wordcraft-cli/src) | 1,901 | `convert`, `text`, `inspect`, `run`, `commands`, `parity`, `mcp` and `zotero` |
| apps | [`wordcraft-web`](../apps/wordcraft-web/src) | 1,159 | Browser build (eframe web runner, WebGPU with WebGL2 fallback) |
| tooling | [`xtask`](../xtask/src) | 1,488 | Workspace maintenance and quality-gate commands, including `layers` |

## Data flow

1. **Open:** `engine::io` picks a reader by extension (`docx`, `docbin`, `formats`, JSON) and
   produces a `doc::Document`.
2. **Edit:** every user-visible action is a command id run through `Session::run` (ribbon,
   shortcuts, command search, CLI, control channel and MCP all dispatch by id). `Session::run`
   snapshots for undo (blocks are `Arc`-shared, so snapshots are cheap) and catches an escaped
   panic, restoring the document (never-crash standard). After the command it gives the blocks
   the command made or copied ids of their own (`doc::BlockId`, random, never saved): a block
   keeps its id while it is edited, so a co-edited change can say which blocks it touched.
   `Document::ensure_block_ids` compares the edited document with the snapshot `Session::run`
   took before the command and looks only at the blocks that aren't the same allocations.
   When the command ends, what it changed becomes one change (`doc::ChangeSet`) in the
   document's change log (`engine::ChangeLog`, the last 1,024): the blocks taken away and put
   in, by id, with a text splice for mapping positions, table properties, parts and document
   fields; a paragraph edited in place sends only the characters the edit replaced, with their
   formatting and objects. Another copy of the document makes the same edit with
   `Document::apply`, which checks by id and content fingerprint that it holds what the change
   was made on, and is left as it was otherwise. A change made at the same time as another is
   moved past it with `ChangeSet::rebase` (past the characters the other put in or took away in
   the same paragraph), unless the two change the same characters or blocks. Agents follow the
   log (`document.changes`, MCP `get_changes`).
   Every command has a scope (`engine::scope`: document, selection, view, file, app or UI),
   what it touches and so who may run it in a shared document, and names the params that are
   places in the document, which a change moves (`ChangeSet::map`).
   A document people and agents edit at once is a room (`engine::room`): each participant
   edits a copy of their own (`room::Replica`, a `Session`), whose commands change it at once,
   and sends each command's change to the room's hub (`room::Hub`) with the version it was
   made on. The hub accepts a change made on its latest version as it is, moves one made on an
   earlier version past the changes it accepted since, and sends back one that clashes with
   them; every copy makes the accepted changes in that order. A copy moves the changes the hub
   hasn't accepted yet past the others' as those arrive (`Session::apply_change`, which moves
   the caret and selection with their text), and makes its edits that clash again after the
   other's change: it runs the same commands from the same places, moved through that change.
   Each participant has a role (`room::Role`: viewer, commenter, suggester, editor or owner).
   The room's own process joins people and agents under a name and a role, a name no one in
   the room has (ignoring case); anyone else joins with a key the room handed out for one
   (`Room::invite`, `Room::join_with`). A suggester's commands make tracked changes in their
   name whatever the document's Track Changes setting. Their copy undoes a command that
   changes more than the role lets them (`Session::role`), Repeat, a macro, Undo and Redo as a
   whole, and the hub sends such a change back (`room::rules`): a commenter's must leave the
   document as it was but for their own comments (each with a comment part of its own), a
   suggester's the same once every tracked change is rejected, with others' tracked changes
   covering what they covered, where they were, once the suggester's own are taken back (a
   paragraph they split joins its other half again, as Reject does it). Both compare only the
   blocks and parts the change touched.
   The app's window shares the document it shows (`Room::host`, control method `room.share`):
   its own session is the owner's copy, lent to that copy for each command and each frame
   (`Replica::lend`), so the person's typing goes through the hub like anyone's and comes back
   made again, not lost, when it clashes with an agent's. The hub wakes the window when it
   takes a change (`Room::on_change`), and every frame the window brings in what others
   changed. An agent connects to the control port with an invite's key (`room.invite`); the
   control server finds the room in the lobby it shares with the window (`room::Lobby`) and
   gives the connection a copy of its own, which answers it on the connection's thread
   (`room::hosted`): the agent's commands never wait for the window's frame, nor the window's
   for an agent. MCP's `--connect` bridge with that key gives the agent its copy through MCP.
   Revoking the invite or closing the room takes their copies out, and their connections end.
   Each participant's caret and selection reach the others (`room::Presence`): a copy tells the
   hub where they are as anchors in the version it has (`Replica::present`; the window while
   it shows the document, an agent's copy after a request that moved them), and a reader gets
   each place moved by the hub through the changes since to the version the reader has, then
   through its own edits the hub hasn't given back (`Replica::others`), so it never sees a place
   ahead of or behind its document. The hub moves a place only when someone reads it, or before
   it lets go of the changes the place is at, through each change once, and keeps 64; no text
   goes with it. Each name gets one of eight colours, theirs again when they come back unless
   someone else has it by then (`room::Seen`).
   Undo stays each participant's own. Undo steps are snapshots of the whole document, so the
   newest undo and redo steps of a copy keep the changes others made since
   (`Session::apply_theirs`): Undo makes the step's change undone in the document as it is,
   moved past theirs as the hub moves an edit (`ChangeSet::rebase`), and leaves theirs in, or
   refuses and names who changed what the step changed. What the participant types after
   another's change is a step of its own. A copy that makes its edits again after a clash puts
   back the steps it had before them, so Undo among them takes the same step.
   The built-in assistant (`assist`) is one more participant: a conversation takes an invite
   of its own from the window's room, under its backend's name ("ChatGPT", "Claude Code"…; a
   second conversation is "ChatGPT (2)") and a role from the `assistant.defaultRole` setting
   (suggester unless the person chose viewer, commenter or editor; never owner), and gives it
   back when the conversation ends. Its turns run on a thread of its own: a backend that runs
   the model in WordCraft calls the MCP server's tools (`wordcraft_mcp::tools`) on a copy the
   conversation holds (`room::hosted`, so scopes and roles hold as they do for agents), and the
   window reads the conversation's events each frame, never waiting for it. ChatGPT is such a
   backend: the model loop runs in WordCraft, over HTTPS to OpenAI's Responses API with the
   person's ChatGPT sign-in, and its tool calls run on the conversation's copy. An ACP agent
   (Claude Code, Codex, Pi or one the person adds) runs as a process of its own that WordCraft
   talks to over stdio; it reaches its copy through the room's port on 127.0.0.1 with the
   conversation's invite key, as an MCP server (`wordcraft-cli mcp --connect`) or, for an agent
   without MCP, `wordcraft-cli call` from its shell. Each turn starts with the copy selecting
   what the person selected in the window. Stop ends a turn at once, and the agent's process
   if it doesn't stop; an agent that dies ends its turn with an error and starts again on the
   next. Desktop only: the web app's pane says so. A gate test (`apps/wordcraft-cli/tests/it/gate.rs`) has the person, ChatGPT (at a
   fake of its servers) and an ACP agent (a scripted fake) edit one document at once and checks
   that every copy ends with the hub's document, each assistant's edits are tracked and
   reviewed in its name, Undo takes back only the person's steps, stopping or killing one
   assistant leaves the others, and no frame waits.
3. **Layout:** `layout` turns the document into pages of positioned glyphs, boxes and images
   (display list), incrementally: an edit relayouts only what changed (1.4 ms on a 188-page
   document, measured 2026-10-06).
4. **Draw:** the UI paints pages through `render` (CPU raster, cached per page, pixel-aligned) on
   an egui/wgpu canvas; PDF export walks the same display list through `pdf`.
5. **Save:** `engine::io` picks a writer by extension.

## Testing and gates

- 821 `#[test]` functions and 10 property-test blocks: model, layout, DOCX round-trip and hostile
  input, `.doc`, formats, typing parity (`engine/src/tests/typing.rs`), bidi, i18n catalogs, UI
  interaction (egui_kittest), MCP acceptance; `hostile_params_never_panic` runs every command with
  junk params.
- `cargo xtask ci`: fmt, clippy `-D warnings`, tests, asset attribution, layering, wasm build.
- `cargo xtask parity` regenerates [`parity-checklist.md`](parity-checklist.md); a test keeps the
  catalog percentage above a floor that only rises.
- GitHub Actions on PRs: packaging lint, and FreeBSD / Windows ARM64 when build files change;
  release builds on pushes to `release`.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-07 | trivial | Data flow: each participant's caret and selection reach the others as anchors (`room::Presence`): a copy tells the hub where they are as of the version it has, and a reader gets them moved through the changes since and its own pending edits; the hub moves a place only when it is read or before the changes it is at are let go, keeps 64, and gives each name one of eight colours (A1.5, #32) |
| 2026-11-06 | trivial | Data flow: ChatGPT runs its model loop in WordCraft over HTTPS with tools on its copy; ACP agents are processes over stdio that reach their copies through the room's port (`wordcraft-cli mcp --connect`, or `wordcraft-cli call`); Stop, a dead agent, desktop only; the gate test (AS4.1, #27). `assist`'s row: 12,571 lines, and its ChatGPT and ACP backends are desktop only |
| 2026-11-05 | trivial | `assist` at L5 (AS1.1, #16): the built-in writing assistant joins the window's room with an invite and a role (suggester by default, `assistant.defaultRole`, never owner) and calls the MCP server's tools on a copy of its own; `assist` may use `mcp` (same layer, one way) |
| 2026-11-05 | trivial | Data flow: Undo per participant: a copy's newest undo and redo steps keep others' changes since (`Session::apply_theirs`), Undo moves the step's change past them or refuses naming who changed the same thing, and edits made again after a clash keep their steps |
| 2026-11-05 | trivial | Data flow: the window shares its document through a room (`Room::host`, `Replica::lend`), and agents with an invite's key get copies of their own on their connections' threads (`room::Lobby`, `room::hosted`) |
| 2026-11-05 | trivial | Data flow: a participant's Repeat, macros, Undo and Redo are held to their role as a whole; a name in a room is one participant's (ignoring case); a comment's text is a comment part of its own; others' tracked changes stay where they were, not only as they were |
| 2026-11-05 | trivial | Data flow: participants in a room have roles (`room::Role`); a suggester's commands make tracked changes in their name, and their copy and the hub refuse a change beyond the role (`room::rules`), comparing only what it touched |
| 2026-11-05 | trivial | Data flow: a document edited at once is a room (`engine::room`): each participant edits a copy of their own, the room's hub accepts, moves or sends back their changes, and copies make their clashing edits again after the other's |
| 2026-11-05 | trivial | Data flow: a paragraph edited in place sends only the characters the edit replaced (`doc::change::ParaEdit`), and a change made at the same time as another is moved past it unless both change the same characters or blocks (`ChangeSet::rebase`) |
| 2026-11-05 | trivial | Data flow: every command has a scope (`engine::scope`: what it touches, so who may run it in a shared document) and names the params that are places in the document |
| 2026-11-05 | trivial | Data flow: what a command changed becomes one change (`doc::ChangeSet`) that another copy applies (`Document::apply`) and agents follow (`document.changes`, `get_changes`) |
| 2026-11-05 | trivial | Data flow: every paragraph and table has an id of its own (`doc::BlockId`), kept through edits and given after every command to the blocks it made or copied |
| 2026-11-02 | trivial | craft-proof's row: without the Language data English spelling falls back to an installed Hunspell English or the built-in Moby list, and only pattern hyphenation is off (#5202 review) |
| 2026-11-01 | minor | `proof` left the workspace: it is `craft-proof` in storytold/craft-libs (with its history), and the spelling and hyphenation data is craft-i18n's `language-data/` (`CRAFT_I18N_DIR`); `assets/spelling` and `assets/hyphenation` are gone |
| 2026-10-28 | major | Recounted workspace crates, layers and Rust source lines; refreshed proofing languages and documented current engine/layout module boundaries and source links |
| 2026-10-10 | minor | `proof`: proofing languages (English, Polish), affix engine, Polish patterns; layout and engine route each run by its language |
| 2026-10-10 | major | First committed architecture doc, from the crates' sources and `AGENTS.md` |
