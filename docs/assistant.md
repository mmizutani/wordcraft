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

The sign-in is kept in `chatgpt-credentials.json` in the settings folder, which only you may
read (mode 0600 on Unix), never in a document, a recovery copy or the log. Signing in and out
are yours alone: `assist.chatgpt.signIn`, `assist.chatgpt.cancelSignIn` and
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
or nothing: the agent's own name and version aren't a model.

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
outline, held to 32 KiB in all; the first turn adds a note that says which document it works
on, its name and role, and how to use the `wordcraft` tools (for Pi, `wordcraft-cli call`).
Through those it can read the whole shared document.

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
   is signed in and that WordCraft may use your plan, and the settings folder has
   `chatgpt-credentials.json`, which on Unix only you may read.
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
4. Sign Out on the same page: `chatgpt-credentials.json` is gone, and the pane says to sign in,
   with Continue with ChatGPT under it.

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
