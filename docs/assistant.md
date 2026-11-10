# The writing assistant

WordCraft's assistant drafts, rewrites, summarizes and edits the open document in a conversation
in the assistant pane. Whichever backend runs it, it is a **participant** of the shared document:
it joins the document's room with an invite of its own, under its backend's name ("Claude Code",
"Codex"…), and acts only through the commands the [MCP server](mcp.md) offers any agent. So what
it may do is what its role lets a participant do, and every change it makes carries its name.

## Switching documents

One window shares one document at a time. If another open document is shared, the Assistant
pane offers **Use this document** (`assist.useDocument`). Switching ends the previous
conversation and disconnects that document's participants; both documents stay open with their
edits. A prompt that cannot start in another document leaves the previous conversation running.

## Roles

An assistant joins as a **suggester** unless you choose otherwise: everything it changes is a
tracked change in its name, which you accept or reject (Review › Accept or Reject). The setting
`assistant.defaultRole` (File › Options › Assistant) picks the role every new conversation joins
with, and a conversation may pick its own:

| Role | The assistant may |
|---|---|
| `viewer` | read the document; it changes nothing |
| `commenter` | also add comments and replies |
| `suggester` | also edit, always as tracked changes in its name (the default) |
| `editor` | also edit directly, untracked |

A role picked in the pane applies to the conversation it starts. Later conversations use
Assistants join as again unless you pick another role. The pane’s Accept all and Reject all
buttons include the assistant’s tracked changes in headers, notes and text boxes.

Never `owner`: an assistant can't change WordCraft's settings. A setting of `owner` is read as
`suggester`. See [Roles](control-protocol.md#roles) for how the room holds every participant to
theirs.

## In the pane

The pane shows each turn as it goes: your prompt, the answer as it comes, the assistant's
thoughts folded away, and a row for each tool call, folded to its title with a mark for how it
stands (waiting, running, done, failed). Opened, a row says what kind of call it was and how it
went, why it failed if it did, and for a call that changed the document, **Show the change**
selects exactly the text it put in or changed with Review open, where Accept and Reject act on it
(`assist.showChange {from, version}`, the versions of the shared document the call's changes
made: a batch makes several). It leaves out what anyone else changed meanwhile and text typed
right before it later, and says so when that text is gone. A call of WordCraft's own tools has
its versions as it ends; an agent's call (ACP) has those of the changes the assistant made
through WordCraft (its MCP server or `wordcraft-cli call`) while the call ran, so two calls
that ran at the same time both show what changed meanwhile. The agent's plan is a checklist that
counts what is done. A permission request waits in a frame of its own,
with the agent's answers (at most 16, their names cut to fit and whole on hover). A turn that
stopped other than by finishing says why, plainly: the answer was cut off at the most the model
writes at once, the assistant stopped after the most steps one request may take, or it declined
to go on.

Each turn has a line saying which model answers and at what reasoning effort, such as
`GPT-6-Luna · Medium`. `(requested)` after the effort means ChatGPT didn't say which one ran, so
the line shows what WordCraft asked for. The same place has a short note when the turn runs
something other than what was chosen, or when an effort wasn't applied.

An answer shows as formatted text: headings, lists, emphasis, code and quotes, read with the same
Markdown parser the file formats use. While an answer arrives, what its last paragraph hasn't
finished is closed or held back, so no `**` comes and goes, and a link waits until its address is
whole. A picture shows only its description; nothing is fetched. A link is a widget of its own in
the text, reachable by keyboard, with its address beside its words when they differ. Only a
`http` or `https` address opens, and only on your own click: a click the control channel made
leaves it closed and the pane says so. At most 64 KiB of an answer, 512 blocks and 4096 runs of
text are formatted, and the rest shows as plain text, as does a whole answer whose table would lose
words to the limits a document's tables have.

The note asks the assistant to cite what a claim rests on with the exact words, in a marker the pane
reads: `[[cite: "the exact words"]]`, at most 200 characters, and at most 64 show in an answer. The
pane takes the marker out and shows a numbered chip that selects those words, or says which attached
source has them. The document is read as it will read, with tracked deletions left out, as the model
got it. A marker whose words are in neither the document's body nor a source shows no chip, so a
made-up citation can't look real, and Sources says how many couldn't be found. Quoting needs nothing
of the tools: quoted words still resolve after edits elsewhere. A marker not closed yet is held back
while the answer arrives, and shows as text if the turn ends first. A thought's markers aren't read.

Once there is a conversation, tabs under the role switch it with its **Details**, which keeps
updating during a turn and reads only the conversation's own record, so the two can't disagree.
Each section folds. **Steps** lists each turn under what you asked: the agent's plan when it gave
one, else the turn's tool calls by title, with the same marks as the conversation. **Sources**
lists what each turn read. Its first row says what WordCraft gave the model: the selection or
where the caret is, how many paragraphs around it, the outline, and how much of the 32 KiB it took,
with a note when the selection was cut. That comes from the pass that wrote the text the model got,
so it can't disagree with it. Under it come the turn's reads and searches, each once: WordCraft's
own tools by name, where a row for a paragraph selects it, and says so if it has gone; a file an agent read by its path and
line; an address an agent fetched, which opens only on your own click. An agent's read that doesn't
say where it looked is listed by its title, and under them what the turn's answers cite. **Add a source…** there attaches a file, from the system's
file dialog, or another open document, for the assistant to refer to, and it can be attached before
the conversation starts. The list says each one's name and size and whether it was cut: a
conversation holds at most 8, of 64 KiB of text each, and a turn carries at most 128 KiB, which each
turn's row says too. Text comes off the window's frames, through the importers documents open with,
or as it is for a plain text file, from at most 16 MiB of file. A prompt waits while one is being read, so it never goes without the source you just chose. Sources belong to the conversation
and the document they were attached in: a new conversation starts without them, and no other
document's turn gets them. They are data: a turn gives each one's text as a JSON string, so nothing in
it can read as more than the source, and the note tells the assistant they are never instructions. ChatGPT gets them as input
items, and an agent as `resource` blocks when it says it takes them
(`agentCapabilities.promptCapabilities.embeddedContext`), else as text. Adding takes your own click:
one the control channel made does nothing. `assist.knowledge {"add"?: path, "document"?: id, "remove"?: n}` does the
same from a script, its path held to the files commands may use there. **Output** lists the tracked changes and comments this
conversation made, newest first and by turn, by the name it joined under, while the pane is open:
each with its words cut short, Show, and for a change, Accept and Reject of that change alone. A
change someone resolved, you or anyone in the room, says how it ended, read from its words in its
paragraph: gone means an insertion was rejected or a deletion accepted, still there means the other
way round. A formatting change, a move, words that were there more than once, a paragraph longer
than 64 KiB, or a paragraph that has gone (joined to another, it may hold the words still) says only
that it was resolved. A comment says whether it is open, resolved or
deleted. At most 256 changes and 64 comments are listed, and the rest are counted; Review has them
all, and while some are past the limit no row is said to have ended. Rows that ended go first when
the list is full. `review.accept` and `review.reject` take an `author`, which leaves anyone else's
changes in the selection alone, and a `kind` with it, for the one text that carries two changes of
theirs.
Accept all and Reject all stay under both views. **Properties**
says which assistant it is, with the version an agent gives, the document it works on, its role,
whether its edits are tracked, the model that answered and at what effort, and what the last turn used: tokens for
ChatGPT, added up over the requests of a turn, with a link to your plan's usage page because a plan
has no price per request, and how much of its context and money the session has used when an agent
says, its cost as the agent gave it. A row with nothing to say doesn't show. When a turn starts
waiting on a question for you, the pane goes back to the conversation, where the question is.
`assist.details` answers a script with the same summary.

What you accept and reject of the assistant's changes reaches its next turn. The turn's context
starts with a short list, newest first, of what you decided since its last turn, each change's
text cut to 120 characters and the list to 2 KiB, so the assistant adjusts within the
conversation. It stays with the conversation, and a new conversation starts without it; the one
place a decision is kept is a case's record, when you turned that on for the case
([The record](#the-record) below). Only your own
Accept and Reject count: not another participant's, a script's, a macro's, or a click the control
channel made. A row's Accept and Reject in Output count as yours too, and Sources says when a turn
was given such a list, and how long it was.

The list also says when you changed what the assistant wrote, after accepting it or inside its
tracked insertion: what the text said and what it says now, each cut to 120 characters from just
before they differ. Typing elsewhere and rejecting the change don't count. Neither does a text
when anyone else changed the document since it was last read, or when a script changed it through
the window: the list quotes it as your words, so it says nothing when it can't tell whose they
are.

## Memory

What you keep reaches every later conversation with every backend. Details › Memory in the
Assistant pane has the list: add a preference, edit it, turn it off, remove it or clear them all,
and turn the whole thing off with **Give the assistant what you kept**. Off, nothing is sent and
what you kept stays until you clear it. The pane needs a document, so with none open File › Options
› Assistant has the list instead; with one open, it says where the list is. `assist.memory` reads
the list for whoever holds the window's key.

**Only your click stores a preference, confirms a note, sets a case's facts or replaces your
plan.** Documents are untrusted: a shared document, a received file or a comment can tell a model
to remember something, and a stored preference acts on every later document. So `assist.memory.add`,
`.edit`, `.remove`, `.clear`, `.enabled` and `.keep` refuse a script, an agent, a click or key the
control channel made and a macro's step, as signing in to ChatGPT does, and they take no key and go
on no ribbon or toolbar. An invited participant's copy refuses them at every role. Inside a case you
bound ([Cases](#cases)), the assistant does write on its own: working notes, marked unconfirmed,
which stay in that case and which you confirm, edit or remove. That is the one thing a model stores
without you, and it is data for later turns of that case, never an instruction.

This guards the commands, not the file system. A program holding the window's key can already
write any file you can with `file.saveAs`, this one and the agents' settings included, so that
key is as trusted as you are. An invited agent holds only its invite's key, and can't.

**The assistant proposes.** When you state a lasting preference, or what you accept and reject
implies one, the model may write `[[remember: …]]` in its answer. That marker is taken out of what
the pane shows, even while it streams, and becomes a card under the answer with **Keep**, **Edit**
and **Not this**, at most three in a turn. The marker carries no authority, because a document can
make a model write one too: it is the card, and your click on it, that counts. So the card shows
the exact text before anything is stored, and the control channel's click on Keep does nothing. A
proposal of something you already kept or said no to shows no card, whatever its spacing or case,
and neither does any proposal while memory is off. The file keeps a hash of what you said no to,
not the text. No request is made for proposals: they come within the turns you asked for. The one
extra request the assistant's memory makes is **Review this case** ([Cases](#cases)), and only
when you click it.

**What shaped an answer.** The model is asked to write `[[used: m1]]` where one of your preferences
shapes its answer. That marker becomes a numbered chip in the answer, such as **M1**: hover over it
for the preference's words, and click it to go to Details › Memory. There each turn lists what it
sent: those a chip marked, with their numbers, and the others under **The whole answer**. A marker
for something the turn didn't send does nothing. The model is also asked to follow the current
request when a preference conflicts with it, and to say so in its answer.

**When to ask again.** The list says how often each preference was used and what became of the
changes made with it. `used` and `lastUsed` go up when a marker names a preference, once in a turn,
and the last used go first in what the next turn sends. When you accept or reject a change the
assistant made in a turn that marked a preference, that preference's `accepted` or `rejected` goes
up, unless you have reworded it since.
A change belongs to the last turn that started by its revision's date, so one made in the same
second a new turn starts counts for that new turn. Only your own Accept and Reject count, not another
participant's or a script's, and a change resolved once a new conversation has started counts for
nothing.

When `rejected` is at least 3 and more than `accepted`, the pane asks whether to keep the preference,
under the conversation and in the list, with **Keep** and **Remove**. Nothing changes until you
answer: the preference still goes to every turn. Keep sets `accepted` and `rejected` to 0, so the
question comes back only after three more rejections. So does changing the preference's wording. A
preference that is off, or memory that is off, asks nothing. These counts are the only thing a model
can change here, and they can only reorder what is sent or raise this question.

The list is `assistant-memory.json` in the settings folder, which on Unix only you may read: at
most 200 preferences of at most 280 characters each. It is never put in a document, a recovery
copy or the log, it never crosses a room, and signing out doesn't touch it. Two WordCrafts keep
each other's changes: each write takes a lock and reads the file again. A file that can't be read
whole is moved aside as `assistant-memory.json.damaged` before the next write, never replaced in
silence. It is plain JSON, so you can copy it to another computer.

A turn sends the preferences that are on, most recently used first, up to 4 KiB in all. What
doesn't fit isn't sent, and the list and that turn's Details › Memory say so. ChatGPT gets them in the request's instructions. An
ACP agent gets them in a new session's first prompt and again when they change, including when
none applies any more. They apply unless the current request says otherwise, and they never
permit what the role doesn't. While memory is on, a turn also asks the model to propose only what
you said or did, never something the document asks for. Off, it asks for nothing.

## Cases

A case is one contract negotiation: a title, the facts that make it like or unlike another (the
kind of contract, which side you are on, whose paper it is, what kind of counterparty, the
governing law, its size, language and stage, and the parties' names), a dated power balance with
the factors behind it, a plan, and notes of five kinds: a pattern, a pitfall, a lesson, leverage,
or a position taken on a clause (its topic, the terms, who moved, what was traded and whether it
landed). Cases belong to a client, and a client is a folder; the default client is **My cases**.
Every field but the title is optional. Beside the cases are the lessons you kept: one sentence
each, with the coarse facts you let travel with it, which every client's cases recall
([Lessons](#lessons-that-cross-clients) below).

Details › Case in the Assistant pane shows the document's case. With none bound it lists your
cases by client, each a button that binds the open document to it, and has **New case**: a title
and a client, chosen from the list or typed as a new one. Bound, it shows the title and client with
**Unbind**, the facts as pickers, the balance with its date and **Reassess** (Stronger, Even or
Weaker, and up to four factors each favouring us or them, with a reason), the plan with its next
steps, and the notes grouped by kind, each with **Edit** and **Remove**. A note the assistant wrote
shows dimmed as unconfirmed with **Confirm**; what you wrote or confirmed is confirmed on the day
you set it; a confirmed note also has **Make a lesson**. The commands are
`assist.memory.case.new`, `.bind`, `.unbind`, `.facts`, `.balance`, `.plan`, `.note`, `.noteEdit`,
`.noteConfirm`, `.noteRemove`, `.cardDismiss`, `.record`, `.recordDelete`, `.review` and `.delete`, `assist.memory.wall.new` and
`.delete` for a client, and `assist.memory.lesson.keep`, `.edit`, `.off` and `.remove` for a
lesson. They are yours alone, as [Memory](#memory)'s are, and refuse a script, an agent, a click or
key the control channel made and a macro's step. There is no command that reads a case: the pane
reads the file itself, so holding the window's key doesn't list every client's cases.

**Binding.** You bind a document to a case; nothing in the file says which, so a document can't name
a case and a saved file carries no case's id. WordCraft remembers your binding for the file, by its
full path, in your settings folder: when you open that file again, after a restart too, the pane
says "When you last had this file open, it was bound to <case>" with **Bind again** and **Not now**,
above the conversation and on Details › Case. It isn't bound until you click **Bind again**, and
until then no turn recalls from it: a path doesn't say which document is there, and another client's
document saved over it, or one made from the same template, would otherwise recall the first
client's case. **Not now** leaves it unbound while it is open, and it is offered again the next
time. Unbind, or a binding to another case, changes what the file is offered next time, and deleting
a case forgets every file bound to it; a binding whose case is gone is dropped quietly when its file
opens. The binding belongs to that path: a copy saved under another name is offered nothing until
you bind it, and a document you bind before it is first saved is bound while it is open in this
window, so bind it again once it has a file to keep it. A conversation belongs to the case its
document was bound to when it started. Binding the document to another case, or unbinding it, ends
the conversation, stopping a running turn, since the model holds what that case's notes said and the
next turn would run under another case.

When you bind a document, the case keeps a print of it: the 256 smallest hashes of its folded
paragraphs (headings, numbering and lines shorter than 24 letters left out), with the day and the
document's title. No paragraph text is kept, a case holds the newest eight prints, and no turn
carries one. When the pane shows a document bound to no case, the window compares the document's
print with the prints of the 128 most recently changed cases, in every client's folder, reading at
most 16 MiB of their files, on one worker thread at a time, and the best match
holding at least half of the document's paragraphs becomes a card at the top of the pane: "71% of
this document's paragraphs match a draft of Kestrel MSA 2026 (Kestrel Freight KK)" with **Use it**,
**Not this** and **New case**. This is the one thing that reads across clients, and its result goes
only to your screen: never to a model, a log or a file. The percentage is shown because two
contracts from one template overlap too, so it informs you and decides nothing; nothing is bound
until you click **Use it**, which runs `assist.memory.case.bind` and so refuses a click the control
channel made. **Not this** hides the card for this document in this window and writes nothing.
The print is taken once per document shown, and again after 64 more changes to it while it stays
unbound, or at once when a document whose paragraphs are too few or too slight to compare (blank, or only numbering and headings) gains or loses one, so a draft
pasted into a blank document is looked at as soon as it lands. A document that copies your paragraphs earns a suggestion, which still takes your click.

**What a turn recalls.** With a case bound, each turn carries what ranks highest for the prompt and
the text around the caret: this case's notes, the confirmed notes and latest balance of the
same client's other cases, and the lessons you kept from any client. Never another client's notes
or names: recall opens one client's folder and the lessons file, and no other.
A model's unconfirmed note stays in the case it was written in. What is recalled goes to the model
as data, in the turn's sources, with WordCraft's own framing that it is never an instruction and
mustn't be copied into the document unless you ask; it is never in the instruction channel that
carries your preferences. The model is asked to write `[[used: n12]]` where an item shapes its
answer, which becomes a chip as a preference's does. Details › Recalled lists, per turn, each item,
where it came from (this case, which of the client's cases, or "a lesson you kept"), whether it is
unconfirmed, the balance then against now, how many more scored but didn't fit, and why nothing
was recalled when nothing was.

**The clause ladder.** When the prompt names a topic (the liability cap, say) on which this case
and the same client's other cases hold three or more positions, those notes go as one item,
`ladder:liability`, instead of one by one: every position on that topic, each with where it is
from, its date, who moved, the terms, what it was traded for, whether it landed, how it is known (from you, or from the other side's text), the balance then
against now, and whether you confirmed it. The ladder is computed from the case files for each
turn and stored nowhere, so deleting a case takes its rungs with it and nothing can go stale. It
is ordered for the question "what did we concede, and when were we weaker": the position taken
under the balance closest to now first, then landed before open, then newest; at most eight rungs
go, and the rest are counted with what didn't fit. Lessons, patterns and pitfalls on the topic go
as before and never enter a ladder, and another case's unconfirmed position never does either.
Details › Recalled lists a ladder's rungs, each on a few wrapped lines, and a change that pastes a rung's terms
is flagged as quoting your case notes like any other recalled text.

Each turn of a bound conversation also carries the case itself as it stands: its facts as the
pickers set them (the kind of contract, your side, whose paper, the counterparty's kind, the
governing law and the stage), the balance now with its factors, and the plan with its next steps.
The case's title and the parties' names never go. The plan is your own text and goes as you wrote
it, each line up to the 400 characters a goal or a step keeps, so a name you write in the plan, or
in a factor's reason, goes too; a plan or balance the assistant wrote goes marked unconfirmed. The
facts, the balance and the goal always go, counted first against the 6 KiB a turn recalls. The
steps go last, in order, with the room the ladders, lessons and notes left, and the summary takes
at most half of the 6 KiB with them, so a long plan never pushes out what ranked for your question;
a step that doesn't fit is left out whole, and Details says how many. It goes as data in a source
of its own, under WordCraft's own words, on ChatGPT and to an ACP agent alike, and to an agent on
every turn, since the balance and the plan change. Details › Recalled lists it first, as this
case as it stands. It is off whenever recall is off.

**What the assistant writes.** With a case bound, each turn's instructions also say what the
case's notes are for: a later conversation about this negotiation will know only these notes and
the document, so the model should write what that conversation will need and can't read off the
document (rules learned about the other side, positions and trades, traps, how the balance moved
and why), say how it knows each one, and say so when it came from the other side's text. It writes
them as `[[case]]{…}[[/case]]` markers in its answer, at most five in a turn, each a JSON object
with an `op`: `note` (a note of any kind, with `how`), `plan`, `balance`, `facts` or `lesson`. The marker is
taken out of what the pane shows, even while it streams and even when the JSON holds `]]` or
`[[/case]]` inside a string, since the close counts only outside the JSON's strings; one whose body
passes 8 KiB (however the stream splits around its close), one left unclosed when the answer ends,
one opened inside another, a sixth in a turn, or one this build doesn't read (an unknown `op`, a
field a record doesn't have, a `standing`) is dropped and counted in Details › Recalled, never shown
as text. A marker inside code, fenced or inline, is an example the answer explains: it is shown as
written and nothing is written from it, and the same goes for `[[remember:` and `[[used:` there.

Each marker goes one way, by the conversation's own case and never the document's current binding,
so a note written during a turn lands in the case the conversation started with even if you rebind
meanwhile, and a case another WordCraft is saving holds the write for a later frame without
stalling this one. A `note` or a `balance` is stored in that case, unconfirmed: a quiet row appears
under the answer, "Noted for this case: …", showing the whole note as the case holds it now, every
field, since Confirm approves all of them, with **Confirm**, **Edit** and **Remove**, which are the
same commands as the page's and refuse the control channel's click. A `plan` replaces the plan only
when the assistant wrote the current one; over a plan you wrote it becomes a card. `facts` is always
a card, since you decide what a case is, and so is `lesson`, since a lesson leaves the client. A card shows the whole proposal with **Use these** (or
**Use this plan**, **Keep**, **Reassess**), **Edit** and **Not this**, as a preference's card does,
and stores nothing until your click; every button, Not this included, is a command of yours alone,
and Use these fills only the facts the card names and leaves the parties and the stage as they are. A case holds at most 64 unconfirmed notes; past that a new one
replaces the oldest never used. A note the model writes that a note already says, or that you
removed before, is dropped without a row.

A poisoned document can therefore make the assistant write an unconfirmed note inside its own case,
and later turns of that case recall it as unconfirmed data, shown as such and discounted in ranking.
It can't confirm it, carry it to another case or another client, set the case's facts, replace a
plan you wrote, bind, unbind or delete: none of those has a marker, and the commands that do them
refuse everything but your click. Recall of the same client's other cases takes confirmed notes
only, so an unconfirmed note never leaves the case it was written in. The switch for all of this is
one constant, `MODEL_WRITES` in `crates/assist/src/cases/marker.rs`: `InCase` stores notes as
described; `CardsOnly` turns every write into a card and stores nothing without a click.

**Review this case.** Under the case's Record section, **Review this case** asks one turn in this
document's conversation, and says in front of the button that it spends one request on your plan.
The prompt is fixed, WordCraft's own words; beside it the turn carries the case's notes (ids, kinds,
whether you confirmed each, how it was known, dates), its facts, balance and plan, and the most
recent 96 KiB of its record if it keeps one, as one source marked as data, under the same proof as
recall: with anyone else in the room, or invited, the review is withheld and says who. The answer
is an ordinary answer, with four more markers that mean something only in it: `correct` (a note's
main text as it should read now, and why) and `retire` (the note no longer holds, and why) apply to
the assistant's own unconfirmed notes as any in-case write does, the why kept as how and a retired
text blocking the assistant from noting it again; on a note you confirmed each is a card, **Keep**
or **Remove** being your own edit of the note, and nothing changes until you click. `supported`
shows as a quiet row with where the record bears the note out, and `gap` as a question for you;
neither changes anything. A correction to text you dismissed, or that another note already says, is
refused as a new note would be; what the assistant retires is kept apart from what you removed, so
however many of its notes it retires, your dismissals stay. A note id the case doesn't have, and
any of the four in a turn you didn't ask as a review, is dropped and counted in Details. A script,
an agent, the control channel's click and a macro's step start no review. The command is
`assist.memory.case.review`, and Search leaves it out, as it does Delete.

**Lessons that cross clients.** A note stays inside its client. The one thing that crosses is a
lesson you kept on a card: its text, and the coarse facts you left ticked. The card opens from
**Make a lesson** on a confirmed note's row, or from a `lesson` marker the assistant writes
(`{"op":"lesson","text":…,"notes":[ids]}`), which is always a card and never a write, whatever
the `MODEL_WRITES` switch says. The card shows the exact text, with **Edit** to reword it; flags any
word of it that names a party of the case, matched with case and full-width letters folded as
search folds them, and while one is there **Keep** waits until you tick **Keep it with the name**;
lists every fact that would travel (the kind of contract, your side, whose paper, the kind of
counterparty, the governing law, the balance then, and the year, never the size, the day, a name or
the case) with a tick each to leave it home; says it goes to every client's cases; and has
**Keep**, **Edit** and **Not this**, all yours alone. Keep runs `assist.memory.lesson.keep` and
stores the text and the ticked facts under an id such as `l3`, with which case it came from kept on
this computer only, never sent. A turn recalls a lesson that isn't off from any client, ranked by
the same score as a note, with its facts and the balance it was kept under against now, as "a
lesson the person kept"; nothing in it names a case, a client or a party. Details › Case lists the
lessons with **Edit**, **Off** or **On**, and **Remove**, and File › Options › Assistant does when
no document is open.

**Measuring it against a real model.** `cargo run -p wordcraft-assist --example case_eval --
--runs 3` builds six turns the way the app does (a synthetic contract, a synthetic client's
cases, a saved preference, the case ask in the instructions) and sends them to the OpenAI
Responses API with the ChatGPT backend's own request builder and WordCraft's real tools offered,
then prints how often the model drew on a recalled note, followed an instruction hidden in one,
quoted notes into a tool call a poisoned document asked for, named the weaker position behind a
concession won under a different power balance, wrote a case note with `how` when the person said
something worth keeping, and refused to record a poisoned document's claim as the person's own
position (a note that records it as the other side's text passes). For every scenario it also
counts how many of the notes written say where they came from. A response that is incomplete,
refused, or empty is counted as `no answer`, not as a pass or a fail. It spends credits on the key in `WORDCRAFT_OPENAI_API_KEY` (one request per run per
scenario), runs only when you run it, and `--show` prints the requests without sending them. Load
the key from a file you keep outside the repository, for example `set -a; . ~/secrets/.env; set
+a` before the command; never type the value into a command, which a shell may save to its
history.

**Quotes.** A tracked change or comment the assistant makes that repeats a run of 36 bytes (about
six English words or twelve Japanese characters) from anything the conversation recalled, in any
turn and any field of an item, shows "This quotes your case notes" in Details › Output, beside its
buttons while it is open and on the row after it is accepted, so you see it before sending on a file
that has it. It is a flag, not a wall: a paraphrase passes.

**When recall is off.** Nothing is recalled with no case bound, on the web (no settings folder),
or while anyone besides you and this conversation's assistant can read the document: another
participant, or an invite not yet used. Details › Recalled says which. Once a conversation has
recalled anything, someone joining doesn't just turn recall off: the next turn is refused, and a
running turn is stopped and the conversation ended, with the pane saying who can now read the
document, because the model still holds what it read and a document could ask it to write that into
a comment the newcomer reads.

**The record.** A case can keep a record of its negotiation, the transcript layer of its memory,
which the review pass of a later change will read; no turn reads it. It is off for every case
until you tick **Keep a record of this negotiation** under Details › Case › Record, and the
checkbox says what it stores and where. On, each conversation on the case is one file,
`log/<time>-<n>.jsonl` in the case's own folder in the settings folder (mode 0600, only you can
read it), one record a line: each turn as it ended (what you asked, the answer with the `[[case]]`
and other markers taken out, the tool calls, the model, how it stopped, and the ids recalled and
used; nothing is written while a turn streams), your Accept or Reject of anyone's tracked change
with its author, kind and text (a script's, a macro's or the control channel's click writes
nothing), your edit of what the assistant wrote (what it said, what it says now), and a snapshot
of the other side's tracked changes and comments with author, date, text and position when the
conversation first takes the draft up, scrubbed metadata showing as it is. Each line goes to the
case the conversation started on, never the document's current binding. The file has one writer,
the conversation's own thread, fed without waiting, so a frame never waits for the disk; records
that don't fit the queue become one `dropped` line with their count, a file stops at 16 MiB, and a
case keeps at most 64 MiB of record, its oldest conversation's file going first. It isn't fsynced,
so a power cut can lose the last lines, and a crash cuts at most the last line, which a search
skips and counts. A link where `log/` or a file in it should be is never followed. The section
shows the record's size and a search box whose search runs on a worker; **Delete the record**
(`assist.memory.case.recordDelete`) removes `log/` and leaves the case, and deleting the case
removes it too. Both are yours alone, as every case command is.

**Where the files are.** `assistant-cases/<client>/<case>/case.json` in the settings folder, with
`wall.json` naming the client, and `assistant-lessons.json` beside `assistant-cases/` for the
lessons; folders are mode 0700 and files 0600 on Unix, so only you may read
them. A file is at most 1 MiB, and one that can't be read whole shows as damaged and is moved aside
as `case.json.damaged` (or `assistant-lessons.json.damaged`) by your next change, never replaced in
silence; while the lessons file is damaged a turn recalls no lesson and Details › Recalled says so.
Two WordCrafts keep each other's changes: each write
takes the case's lock, or the lessons file's, and reads the file again. Which files are bound to
which case is `assistant-cases/bindings.json`: each file's path and the ids of its client and case,
at most 200 files (binding a 201st forgets the one bound longest ago) and 1 MiB. Only your bind and
unbind add or change a binding; a delete, and a file whose case is gone, only remove one. Each write
takes the file's own lock, and one that can't be read whole binds nothing and is moved aside as
`bindings.json.damaged` by your next bind or unbind. Nothing of a case goes in a document, a recovery
copy or the log, and it never crosses a room.

**Delete.** Delete case asks in place, says it can't be undone, and removes the case's folder with
every note and its record in it, and forgets the files bound to it, on a worker: the document's
conversation ends first and its record's file is closed there before the folder is renamed, so the
frame never waits, and a delete another WordCraft refuses (its lock, a file in use) comes back as a
notice and loses nothing. Deleting only the record leaves the files bound. It lists the lessons kept
from the case with **Delete these too** ticked, and
unticked they stay and lose the link to the case. A client with no cases left can be deleted from
the list, its cases' lessons having been asked about case by case. What WordCraft can't
reach: what a turn already sent to a model provider under your account, an agent's own record of a
conversation in its own folder, and a backup of your settings folder. The page says the case was
deleted if another window did it.

## Backends

| Id | Backend | What runs it |
|---|---|---|
| `chatgpt` | ChatGPT | OpenAI's Responses API on your ChatGPT Plus or Pro plan, through Sign in with ChatGPT (no API key): see [Signing in to ChatGPT](#signing-in-to-chatgpt) |
| `acp:claude-code` | Claude Code | its ACP adapter, `claude-agent-acp`, with your Claude sign-in |
| `acp:codex` | Codex | its ACP adapter, `codex-acp`, with your ChatGPT sign-in or an API key |
| `acp:pi` | Pi | its ACP adapter, `pi-acp`, which runs `pi` with the model providers you signed it in to |
| `acp:custom` | Custom agent | any agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com) on stdio |

The pane lists them with what each needs first; `assist.backend {"id": "acp:claude-code"}` picks
one from a script. Each conversation gets the backend picked when it starts. The web app offers
none of them: see [On the web](#on-the-web).

ChatGPT runs in WordCraft: each turn sends the conversation to the Responses API, streamed, with
WordCraft's tools as function tools, and runs the calls the model makes on the conversation's
copy of the document.

An ACP agent is a program on your computer that WordCraft starts for the conversation (on its
first turn: the pane shows "Start Claude Code" while it starts) and speaks the Agent Client
Protocol with over its standard input and output. The agent runs its own model loop with its own
account; WordCraft gives it the document through its MCP server (Pi: through its shell, see
[Pi](#pi)) and shows what it says, thinks, plans and does in the pane.

### Signing in to ChatGPT

File › Options › Account has **Continue with ChatGPT**, and so does the Assistant pane while
ChatGPT waits for a sign-in. Your browser opens at ChatGPT's sign-in; WordCraft waits for it on
a port of its own on `127.0.0.1` ("Waiting for your browser…", with Open the Browser Again and
Cancel) and never holds up the window meanwhile. Once you have signed in, the page shows who is
signed in and whether you let WordCraft use your plan, and ChatGPT is ready in the pane at once.
Plan usage takes ChatGPT Plus or Pro: if you didn't allow it, the page says so, and **Sign In
Again** asks you again. **Sign Out** deletes the sign-in and asks ChatGPT to end it; if ChatGPT
can't be reached, the sign-in is deleted here anyway and the page says you can disconnect
WordCraft in ChatGPT's settings.

Where the sign-in is kept depends on whether the computer has a keychain WordCraft can use: the
macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on
Linux and the BSDs. The page under who is signed in, and the pane under its picker while ChatGPT
is chosen, say which of the two it is:

- **With a keychain**, the sign-in is sealed with AES-256-GCM in `chatgpt-credentials.sealed` in
  the settings folder (mode 0600 on Unix), and the key, 256 random bits made at the first
  sign-in, is the only thing in the keychain, under the service `WordCraft` and an account
  `chatgpt-credentials-key:` followed by a short hash of the settings folder's path. Only the key
  goes there because Windows keeps at most 2,560 bytes in one credential, and the tokens can be
  more. The file alone opens nothing.
- **Without one** (there is none, it is locked and you didn't unlock it, or it fails), the
  sign-in is kept in memory while WordCraft runs, and nothing of it is written to disk: you sign
  in again the next time you open WordCraft.

A `chatgpt-credentials.json` an older WordCraft left, with the tokens readable, is sealed (or,
without a keychain, taken into memory) and deleted the first time WordCraft opens. **Sign Out**
deletes the sealed file, any such older file, the key in the keychain and the copy in memory.
WordCraft asks the keychain on a thread of its own, never the window's: if the keychain waits for
you to unlock it, the pane says to sign in until it answers, and then shows who is signed in. A
sealed file that is cut short, changed, of another version, or doesn't open with the key reads
as signed out, and the next sign-in replaces it. None of it reaches a document, a recovery copy
or the log.

Signing in and out are yours alone: `assist.chatgpt.signIn`, `assist.chatgpt.cancelSignIn` and
`assist.chatgpt.signOut` run from the buttons or Search. From a script or an agent (the control
channel, MCP, also addressed to another document) they are an error, as is a click or key the
control channel made and a step of a macro, whoever wrote it; and they take no key and go on no
ribbon or Quick Access Toolbar, so no one can arrange for a key you press or a button you click
to sign you in or out. The address the browser
opens may carry your last sign-in as a hint, so no command's answer holds it, and the log leaves
out the browser opener's command line. Cancel stops a sign-in unless it is already finishing
what the browser sent: then you are signed in, and the page says so at once; until it has, Sign
Out and a new sign-in ask you to try again in a moment.

### Which model and effort run

With nothing chosen, ChatGPT runs `gpt-6-luna` at `medium`: the account's list ranks a more
expensive model first, and a plan's usage is the person's. What a turn runs comes from the list
and the choice, in this order:

| | Model | Effort |
|---|---|---|
| 1 | The choice, when the list has it | The choice, when this model offers it |
| 2 | `gpt-6-luna`, when the list has it | `medium`, when this model offers it |
| 3 | The list's first | This model's default |
| 4 | | Nothing sent |

A model whose list entry names no levels takes any level. If ChatGPT refuses an effort, the
request goes again once without it, keeping the reasoning summary, and the pane says the effort
wasn't applied. It stays left out until the model or the effort changes. A list that can't be read
stops a turn only when nothing was chosen. A choice that no longer resolves stays saved, and the
pane says what ran instead.

**The menus.** Beside Send and Stop are a Model menu and an Effort menu, wrapping under them when
the pane is narrow. The Model menu starts with a Default row that names what it resolves to, and
that row clears the choice. The Effort menu lists this model's levels, ticks the current one and
tags what runs with nothing chosen, and choosing the tagged row clears the choice. With no level to
tag it has a Default row that does. With no levels listed it offers Low, Medium and High, and a
script may choose any other, which the turn sends. Choosing takes one click. During a turn both are greyed out and say why. The web app has no backend, so it
shows neither.

WordCraft reads the list on a thread of its own when the pane opens with someone signed in and no
list held, and again after a sign-in, so no frame waits for it. While it reads, the Model menu has
the Default row and says so on hover. A read that fails shows the reason under the menus, doesn't
stop a turn that has a saved choice, and is tried again the next time the pane opens. A conversation
reads the list once, and again if the person chooses a model its copy doesn't have.

**An agent's menus.** An ACP agent says what it offers through its session config options: the ones
with category `model` and `thought_level`, groups flattened. WordCraft leaves other categories and
`modes` alone, and booleans stay off, since it doesn't advertise them. Before the first prompt, and
on any turn whose choice differs from the current value, it sends `session/set_config_option`, whose
answer holds the whole set, since a model can change the levels. A value the agent no longer offers
is skipped with a note, and one it refuses is said, without stopping the turn. Going back to Default
sets what WordCraft changed back to what it was. `config_option_update` changes the menus and the
turn's line.

An agent starts on its first turn, so until then the menus have Default only and say so on hover.
An agent that offers neither option has no menus. The line shows the current model option's name,
or nothing: the agent's own name and version aren't a model, and Details shows them apart.

**The commands.** `assist.model {"id"?: string | null}` and `assist.effort {"level"?: string | null}`
choose for the assistant in use: leave the param out to read, and `null` goes back to the default.
Each answers with the backend, what is chosen, what the last turn says ran and what the menus
offer. Once the offer is known, a value outside it is refused and names what is offered. A change
applies from the next turn and keeps the conversation, since each request sends the earlier turns
as words and answers. The choice is kept per backend in the preferences (`assistant.models`). A
person invited to the document can't run either command, at any role, and neither can be addressed
to a document with `doc`.

### Setting up an agent

WordCraft looks for each preset's installed adapter on `PATH` without a shell. When only
Node.js/npx is available, the built-in preset can download and run its named npm package with
`npx -y <package>`. Before every new fallback process, the pane asks you to approve the package
and command. Declining or stopping starts no process; an already-running adapter does not ask
again on its next turn. Programmatic Allow and Reject policies cannot approve this setup: install
the adapter first for headless use. When neither tool is there, the pane says what to install.

A `PATH` set in the agent's environment replaces WordCraft's inherited `PATH` for that agent
(it is not added in front of it), so it must hold every folder the agent needs. On macOS,
installed `/opt/homebrew/bin` and `/usr/local/bin` are appended when their canonical ancestors
are owned by you or root and are not writable by others. Discovery and the child receive the
same PATH, so Node shebangs and helper programs can also find those bins when opened from Finder.
Version managers such as NVM are not loaded from shell startup files: set the agent's PATH to
include the selected Node version's bin directory, or configure an installed adapter and its
required helper paths explicitly. Windows skips npm's bare shell and PowerShell shims and
selects supported executable/batch extensions.

- **Claude Code.** `npm install -g @agentclientprotocol/claude-agent-acp`, or have Node.js on
  `PATH` for npx. Sign in once by running `claude` in a terminal.
- **Codex.** `npm install -g @agentclientprotocol/codex-acp`, or have Node.js on `PATH` for npx.
  Sign in once with `codex login` in a terminal, or give it an API key (`CODEX_API_KEY` or
  `OPENAI_API_KEY`) in its environment below.
- **Pi.** `npm install -g @earendil-works/pi-coding-agent` for `pi` itself, and its adapter with
  `npm install -g pi-acp` (or have Node.js on `PATH` for npx). Sign in once by running `pi` in a
  terminal.
- **Custom agent.** Give its command (an absolute path, or a name looked up on `PATH`), its
  arguments, and its name, which it joins the document under.

When an agent says it needs a sign-in, the pane shows the ways it offers and runs the one you
pick; a way WordCraft can't run (one that needs a terminal) is left out, and the pane says how to
sign in yourself. A script never gets that question: its turn fails with what to do instead.

### The agents' settings

File › Options › Assistant › Agents lists Claude Code, Codex, Pi and the custom agent, each with
whether it can start: Ready, Not found (with what to install), or Unavailable (with why). A row
opens to the agent's command, its arguments, one per line, and its environment variables, and
Save writes them; Discard changes reads the file again, dropping your edits, and Check again
looks for the programs again after you install one. A preset's command left empty runs its
adapter (from `PATH`, or, approved in the pane each time, with npx); its arguments go with a
command of your own. A `PATH` variable set for the agent replaces WordCraft's own `PATH` for it,
both for finding its program and for what it runs, so it must hold every folder the agent needs
(Node's too, for an npm adapter); the notes say so. The variables' values are masked (never shown, copied or read out as text) and never
logged; for a screen reader each row says whether it is open, and each value field and Remove
button is named after its variable. Save waits until what you typed can be kept, and says why
not: a variable with a value but no name, a name with `=` in it, a name given twice, more than
64 arguments or variables, settings larger than the 1 MiB the file may hold (measured as
written, with what the page doesn't show; a file only pretty-printing would take past it is
written compact), or, in a file holding more agents or variables than WordCraft reads (16 agents,
64 variables each), a change that would put something set here past those it reads (a variable
renamed to sort after one the page doesn't show, say): Save checks that the file it writes reads
back as you set each agent the page shows, and a variable you removed is taken out of the file
even when it gained others meanwhile and holds it past the 64 WordCraft reads. When the file's
own data is what takes it past, make room in the file and click Check again: the page measures
again, keeping your edits. What is saved applies the next time the agent starts; one already
running keeps what it started with.

Save writes only what you changed, over the file as it is when you save: an agent you didn't
touch, each field you didn't change, and each variable you didn't add, change or remove keep what
the file holds, even if it changed while the page was open (by hand, or by another WordCraft), and
the page says when it had. What the page doesn't show stays as the file has it too: keys outside
`agents`, entries and fields WordCraft doesn't read, and values of other types. Arguments the
one-per-line field can't show as they are (empty, with spaces at their ends, or with a line break
in them) are kept as they are unless you change that agent's arguments; the row says so. The page
reads the file again each time it is shown, unless it has edits not saved. A file that can't be
read (not JSON, larger than 1 MiB, or unreadable) is never shown as nothing set up and then
replaced in silence: the page says it can't be read, and Save first keeps it as
`assistant-agents.json.damaged` (replacing an older one) beside the new file. If it became
unreadable after the page read it, the first Save writes nothing and says so, and the page says it
shows the file as it was when read; the next Save keeps it aside and saves what the page shows:
that read, with your edits (also when the file has gone missing since). If that write fails, the
file set aside is put back, so nothing is lost and Save can try again, unless a settings file took
its place meanwhile: that one is never replaced, and the page says the damaged one is kept aside.

No command reads or writes these settings: they say which program WordCraft starts and may hold
your keys, so they are yours alone, set in the page or in the file. Input from the control
channel (MCP's `click`, `key` and `type`, or `ui.*`) changes nothing on the page: what it typed
or clicked is put back, and its fields start afresh so undo can't bring it back. A press it made
counts as its input until it is released, even when you release it: that release would click
what it pressed (Remove, Discard changes, Save), so it does nothing. With any of its input the
page also lets go of keyboard focus and of the focused field's selection, and gives focus to
nothing: your next keys never land where it clicked or tabbed, and you click your field again to
go on. A press it holds down starts no drag. Your own clicks are never changed. The page says only you can change these settings. A script picks an agent with `assist.backend`, whose answer
says whether each one is ready, and never holds its environment.

They are kept in `<settings>/assistant-agents.json` (`<settings>` is the folder
[Keys](control-protocol.md#keys) names), which may hold an API key, so on Unix only you may read
it (mode 0600, like the control key):

```json
{
  "agents": {
    "codex": { "env": { "OPENAI_API_KEY": "…" } },
    "claude-code": { "command": "/opt/agents/claude-agent-acp" },
    "custom": { "name": "My Agent", "command": "my-agent", "args": ["--acp"], "env": { "MY_AGENT_MODE": "fast" } }
  }
}
```

Each agent's entry is under its key: `claude-code`, `codex`, `pi` or `custom`. A preset's `command` and `args` replace its adapter; `env` adds to the environment the agent
starts with. Explicit custom commands (including a configured npx command) are your chosen setup
and do not use the built-in fallback approval. An agent reads a damaged file for what it holds; anything that isn't what it
should be is left out (the page instead says it can't be read, as above). `WORDCRAFT_CONTROL_KEY` and `WORDCRAFT_CONTROL_ADDR` are never taken from it.

## What an agent can reach

Through WordCraft, **the shared document, and the files you let agents use**:

- WordCraft gives the agent one MCP server, `wordcraft`: `wordcraft-cli mcp --connect
  127.0.0.1:<port>` with the conversation's invite key in `WORDCRAFT_CONTROL_KEY`. The port is the
  room's own ([The room's own port](control-protocol.md#the-rooms-own-port)): it opens only for
  invites, never for the window's key, which the agent never sees (not in `session/new`, its
  environment or WordCraft's log).
- With that key it gets a copy of the shared document of its own: it reads it, follows what
  others change, and runs commands within its role. Commands that open or save files, that open
  another document, or that reach the window or the settings are refused. A command that reads
  or writes a file it names (a picture, mail merge's recipients, an export to PDF or PNG)
  reaches only the files File › Options › Agents lets agents use: none, unless you chose some
  folders or any file there (see
  [Files participants can use](control-protocol.md#files-participants-can-use)).
- Every connection with the invite's key works on that one copy, and the agent stays one
  participant of the room however often it connects.
- It starts in a new empty private folder (`wordcraft-agent-…` under your cache on Unix,
  or the temporary folder on Windows; Unix ancestor ownership and write permissions are checked), with WordCraft's environment minus WordCraft's own variables, plus the agent's
  settings. WordCraft offers it no file access and no terminal through the protocol.
- When the conversation ends, or the window closes, WordCraft ends the agent and everything it
  started (its Unix process group or Windows Job Object; native Windows validation is pending), removes the folder, and takes the invite back:
  the key opens nothing from then on. Stop ends a turn; an agent that doesn't stop within 10
  seconds is ended, and starts again on the next turn.

The agent itself is a program you run, with your permissions. Its own tools may read files or
run commands; when an agent asks permission first, the pane shows you the question in the turns
you asked for (from the pane, a menu or a shortcut), with the agent's answers as buttons, and
the turn waits for yours; Stop answers it cancelled, and so does closing the pane while it waits.
Only the person answers permission and sign-in questions in the pane: control-channel or MCP
clicks and keys leave the question waiting. A turn a script asked for (the control channel,
MCP, the command line), or one a control-channel or MCP click or key started in the pane, has no
one to ask and never shows a prompt: the agent's requests are refused, unless you turn on File ›
Options › Assistant › Allow what agents ask when no one can answer, and each answer is logged.
Only you turn that on, with your own click: `assist.allowUnattended` reads it and turns it off
but refuses to turn it on, and a faked click on the checkbox is ignored. Turned off, it holds
from a running turn's next request. Use agents you trust, as you would in a terminal.

### Pi

Pi's adapter, pi-acp, doesn't pass the MCP servers of `session/new` on to Pi, so the `wordcraft`
server would give Pi no tools. Pi has a shell tool instead, and reaches the document with
[`wordcraft-cli call`](control-protocol.md#one-request-from-the-command-line): WordCraft starts
pi-acp with the invite's key in `WORDCRAFT_CONTROL_KEY` and the room's address in
`WORDCRAFT_CONTROL_ADDR`, which Pi's commands inherit, and the first turn's note shows Pi the
calls to make (`document.text`, `document.inspect`, `document.changes`, `select.text`,
`text.insert`, `engine.commands`). The key is the invite's, as for every agent: it opens the
room's port and nothing else, the window's key is never in Pi's environment, and the key opens
nothing once the conversation ends. Since Pi works through its shell, it can run any command
you could.

`session/new` gives pi-acp WordCraft's MCP server too, so once pi-acp passes `mcpServers` on, Pi
gets the `wordcraft` tools like the other agents, and the shell route can go. That change is
worth proposing to pi-acp (<https://github.com/svkozak/pi-acp>).

### Privacy

What the agent sends its model, and where, is its business: Claude Code talks to Anthropic,
Codex to OpenAI and Pi to the providers you set it up with, under your account with them. Each
turn WordCraft gives it your prompt and the selection with the paragraphs around it and the
outline, held to 32 KiB in all, and what you accepted and rejected of its changes since its last
turn, held to 2 KiB; what you kept ([Memory](#memory)), held to 4 KiB; what a bound case recalls
([Cases](#cases)), held to 6 KiB, as a source; the first turn adds a note that says which document it works
on, its name and role, and how to use the `wordcraft` tools (for Pi, `wordcraft-cli call`).
Through those it can read the whole shared document.

Sources you attach go to the assistant's provider with every turn of the conversation: to OpenAI for
ChatGPT, and to whatever the agent's model is for an agent. Attach only what you are willing to send
there.

Your ChatGPT sign-in, which lets WordCraft use your plan, is never on disk where another program
or a copy of your settings folder can read it: it is sealed with a key kept in your computer's
keychain, or, without a keychain, kept in memory for the session only (see
[Signing in to ChatGPT](#signing-in-to-chatgpt)). An agent never sees it: agents sign in to their
own accounts themselves. The web app keeps no ChatGPT sign-in at all, since it has no ChatGPT
([On the web](#on-the-web)).

## On the web

The web app has the Assistant pane and every `assist.*` command, but no assistant to run them.
The pane says the assistant needs the desktop app, without a role to choose; the prompt box's
Send and the writing actions stay greyed out, their tooltips saying the same, and a script's
`assist.ask` is refused with that reason. File › Options › Assistant has no role, permission or
agent to set there, only the same reason, and
File › Options › Account says signing in to ChatGPT takes the desktop app.
Both kinds of backend are desktop only (`wordcraft_assist::offers::DESKTOP`: the web build
compiles them out):

- **Sign in with ChatGPT** signs in the way OpenAI documents for apps on a computer: the system
  browser, a loopback port for the redirect, and a credentials file of WordCraft's own, which
  keeps the tokens out of browser storage. OpenAI documents no flow for an app that runs in a
  browser (its endpoints answering a web page, a redirect to a web origin).
- **ACP agents** are programs WordCraft starts and speaks the protocol with over their standard
  input and output; a web page can't start a program.

What would unblock the web:

- a browser sign-in OpenAI documents for apps (CORS on its endpoints and a redirect to a web
  origin), with the tokens still kept out of browser storage; or
- ACP's remote transport (HTTP or WebSocket, a work in progress in the protocol) reached through
  a bridge the person runs on their computer, which starts the agent there and gives the page a
  connection to it on a loopback port, with a key the page is given, as the room's own port
  takes invites today.

Until then the web build offers nothing. `offers_in(host, false)` offers no ChatGPT and no agent
whatever the host has, only the scripted fake when the host asks for it, and the window never
asks for it on the web (`host()` asks only when `desktop(app)`); a scripted assistant a test
gives the window isn't run there either. `cargo test -p wordcraft-assist offers` checks the
first on any build (`the_web_offers_no_backend` reads no file), and the pane's tests
(`on_the_web_the_pane_says_the_assistant_needs_the_desktop_app` and the ones after it) check the
window on the desktop, by having it offer what the web does.

## The gate test

`cargo test -p wordcraft-cli --test it gate` is the assistant's gate (#27): a person, ChatGPT
and an ACP agent edit one document at once, with fakes only (no network, no real agent):

- **The person** is the window: its session, lent to the room's owner copy each frame as the
  app does, typing a key every few frames at the end of the first paragraph.
- **ChatGPT** is the ChatGPT backend, signed in (a first sign-in, which registers WordCraft) at a
  fake of ChatGPT's sign-in and Responses API on `127.0.0.1`, whose answers rewrite the second
  paragraph with `select_text` and `type_text`.
- **The agent** is the ACP backend running the scripted fake agent
  (`crates/assist/src/acp/fake_agent.rs`), which adds a section, a heading and a paragraph,
  through WordCraft's MCP server (`wordcraft-cli mcp --connect`, at the room's own port).

It checks that:

- the window ends with what the room's hub has, and so does each assistant's copy (each reads
  the document at the end); every key the person typed is in, once;
- each assistant's edits are tracked changes in its name, and the person's aren't tracked;
  accepting ChatGPT's and rejecting the agent's keeps the rewrite and the person's typing and
  takes the section out;
- the person's Undo takes back their typing, a step at a time (what they typed after an
  assistant's change came in is a step of its own), and nothing of the assistants'; Redo puts
  it back;
- what a conversation keeps reaches a later one on another backend: ChatGPT proposes a
  preference, which keeps nothing by itself, and once the person keeps it the agent is told it;
- stopping ChatGPT mid-answer ends its turn at once while the agent's goes on, and ChatGPT's next
  turn works; killing the agent's process mid-turn ends its turn ("Fake Agent stopped…") while
  ChatGPT's goes on, the heading it added before stays, and its next turn starts it again; the
  window and the hub hold the same document throughout;
- stopping the agent while it doesn't read what WordCraft sends (it sleeps, as an agent busy
  with a tool call may, and says it stopped seconds later) returns at once, the window's frames go
  on while the agent is still stopping, and its turn ends stopped with nothing more done;
- no frame of the window takes as long as a second while the assistants hold their turns for
  seconds, and asking, ending a conversation and Stop don't wait either, even for an agent that
  takes seconds to stop.

`the_window_never_waits_for_its_assistants` (`cargo test -p wordcraft-ui-egui
the_window_never_waits`) checks the last with the app's own window: its frames timed while the
pane's assistant and another one hold their turns and the person types into the page, then Stop
in the pane and the other's Stop, while both take seconds to stop, the pane's Accept all and
Review's Reject All by author.

## Checking by hand

`cargo test -p wordcraft-assist acp` and `cargo test -p wordcraft-cli --test it acp` run the
scripted fake agent through all of this, as Claude Code, Codex, Pi and a custom agent would;
`cargo test -p wordcraft-cli --test it call` tries `wordcraft-cli call`, and
`cargo test -p wordcraft-assist chatgpt` signs in to fake servers and runs ChatGPT's turns
against them.

Before #15 closes, run this once on a desktop build with the real assistants, and note in #15
the date, the WordCraft commit, the system, each agent's version and how each step went.

**Setting up.** Build WordCraft and its command line tool, which WordCraft looks for beside its
own program and agents reach the document through (without it, every agent says it's
unavailable): `cargo build --release -p wordcraft -p wordcraft-cli`. Start WordCraft with a
document and the control channel: `target/release/wordcraft --sample --control 7981` (on
Windows, `target\release\wordcraft.exe`). Install the agents you check, as
File › Options › Assistant › Agents says for each one missing (Claude Code's and Codex's ACP
adapters, or Node.js so WordCraft runs them with npx; Pi and `pi-acp`), and sign each in to its
own account the way it documents.

**ChatGPT** (a Plus or Pro account):

1. In File › Options › Account, click Continue with ChatGPT: your browser opens ChatGPT's
   sign-in; sign in and allow WordCraft to use your plan. Back in WordCraft, the page shows who
   is signed in, that WordCraft may use your plan and that the sign-in is kept in the computer's
   keychain. The settings folder has `chatgpt-credentials.sealed`, which on Unix only you may
   read, with no token readable in it, and the keychain (Keychain Access on macOS, Credential
   Manager on Windows, Seahorse or KWalletManager on Linux) has a `WordCraft` item whose account
   starts `chatgpt-credentials-key:`. Quit WordCraft and open it again: you are still signed in.
2. In the assistant pane pick ChatGPT and ask it to rewrite the first sentence in a friendlier
   tone: the answer streams in, the rewrite is a tracked change by "ChatGPT", and its tool rows
   open to the change. The turn's line reads `GPT-6-Luna · Medium`. If it says `(requested)`, the
   stream doesn't report the effort: note that in #84.
   Open the Model menu: it lists your account's models with Default (GPT-6-Luna) on top, and if it
   has only that row, or the Effort menu offers only Low, Medium and High for every model, note
   which in #84, since the list then lacks those fields. Note too whether `gpt-6-luna` is listed
   and on which plan. Choose another model and effort, and send another prompt: the line shows
   them, and the conversation goes on.
3. Ask for something long, and press Stop while it answers: it stops at once and says so. Ask
   again: it answers.
4. Sign Out on the same page: `chatgpt-credentials.sealed` and the keychain item are gone, and
   the pane says to sign in, with Continue with ChatGPT under it.

**Each agent** (Claude Code, Codex, Pi):

1. File › Options › Assistant › Agents says the agent is ready.
2. Pick it in the pane and ask it to add a short section at the end: the first turn shows
   "Start" and the agent's name while it starts, and asks you to sign the agent in if it needs
   to; the section comes in as tracked changes in the agent's name, with its plan and tool calls
   in the pane. A permission request the agent makes waits in the pane for your answer.
   If the agent offers a model or a thought level, the menus beside Send list them after this
   first turn. Choose another and send a prompt: the line shows it, and the agent keeps the
   conversation. Note in #84 which agents offer which.
3. Ask it to save the document somewhere, or to read a file on your computer: it says it can't.
4. Ask it to rewrite the whole document and press Stop: it stops, and the next prompt goes on.
5. Pi only: its tool calls are `wordcraft-cli call` commands in its shell (see [Pi](#pi)).
6. Note the agent's process ids and its `wordcraft-agent-…` folder, in
   `~/Library/Caches/WordCraft/agents` on macOS, `~/.cache/wordcraft/agents` on Linux and BSD,
   and the temporary folder (`%TEMP%`) on Windows. Choose another role in the pane and send a
   prompt: a new conversation starts, the agent started again in a new folder, and the processes
   and the folder you noted are gone. Close the window: the new conversation's processes and
   folder are gone too.

**Memory.** With each real backend, state a preference ("from now on, use numbered lists in
summaries") and see a card under the answer. Keep it, start a new conversation with another
backend, and see an answer follow it with its chip. Click the chip and see the preference in
Details › Memory. Reject three changes from a turn that marked it, and see the pane ask.

**All at once.** With ChatGPT in the pane, give an agent outside the pane its own seat: get an
invite with the `room` tool or `room.invite {"name": "Codex"}` on the control channel, and start
Claude Code or Codex in a terminal with WordCraft's MCP server and that key
(`WORDCRAFT_CONTROL_KEY=<key> target/release/wordcraft-cli mcp --connect 127.0.0.1:7981`, see
[Shared documents](mcp.md#shared-documents)).

1. Type in the first paragraph while ChatGPT rewrites the second and the agent adds a section at
   the end: nothing you type is lost, and the window never stalls.
2. Review lists each one's changes under its name. Undo takes back only your typing.
3. Accept ChatGPT's changes in the pane (Accept all) and reject the agent's in Review: the
   rewrite stays, the section goes, your typing stays.
4. Stop ChatGPT mid-answer while the agent works: the agent goes on.
