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

Never `owner`: an assistant can't change WordCraft's settings. A setting of `owner` is read as
`suggester`. See [Roles](control-protocol.md#roles) for how the room holds every participant to
theirs.

## Backends

| Id | Backend | What runs it |
|---|---|---|
| `acp:claude-code` | Claude Code | its ACP adapter, `claude-agent-acp`, with your Claude sign-in |
| `acp:codex` | Codex | its ACP adapter, `codex-acp`, with your ChatGPT sign-in or an API key |
| `acp:pi` | Pi | its ACP adapter, `pi-acp`, which runs `pi` with the model providers you signed it in to |
| `acp:custom` | Custom agent | any agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com) on stdio |

The pane lists them with what each needs first; `assist.backend {"id": "acp:claude-code"}` picks
one from a script. Each conversation gets the backend picked when it starts.

An ACP agent is a program on your computer that WordCraft starts for the conversation (on its
first turn: the pane shows "Start Claude Code" while it starts) and speaks the Agent Client
Protocol with over its standard input and output. The agent runs its own model loop with its own
account; WordCraft gives it the document through its MCP server (Pi: through its shell, see
[Pi](#pi)) and shows what it says, thinks, plans and does in the pane.

### Setting up an agent

WordCraft looks for each preset's installed adapter on `PATH` without a shell. When only
Node.js/npx is available, the built-in preset can download and run its named npm package with
`npx -y <package>`. Before every new fallback process, the pane asks you to approve the package
and command. Declining or stopping starts no process; an already-running adapter does not ask
again on its next turn. Programmatic Allow and Reject policies cannot approve this setup: install
the adapter first for headless use. When neither tool is there, the pane says what to install.

The agent's configured `PATH` takes precedence over WordCraft's inherited `PATH`. On macOS,
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

File › Options › Assistant edits each agent's command, arguments and environment. They are kept
in `<settings>/assistant-agents.json` (`<settings>` is the folder [Keys](control-protocol.md#keys)
names), which may hold an API key, so on Unix only you may read it (mode 0600, like the control
key):

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
and do not use the built-in fallback approval. A damaged file is read for what it holds; anything that isn't what it should be is
left out. `WORDCRAFT_CONTROL_KEY` and `WORDCRAFT_CONTROL_ADDR` are never taken from it.

## What an agent can reach

Through WordCraft, **the shared document and nothing else**:

- WordCraft gives the agent one MCP server, `wordcraft`: `wordcraft-cli mcp --connect
  127.0.0.1:<port>` with the conversation's invite key in `WORDCRAFT_CONTROL_KEY`. The port is the
  room's own ([The room's own port](control-protocol.md#the-rooms-own-port)): it opens only for
  invites, never for the window's key, which the agent never sees (not in `session/new`, its
  environment or WordCraft's log).
- With that key it gets a copy of the shared document of its own: it reads it, follows what
  others change, and runs commands within its role. Commands that open, save, print or export
  files, that open another document, or that reach the window or the settings are refused.
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
run commands; when an agent asks permission first, the pane shows you the question. A
conversation started from a script (control channel, MCP, command line) never asks: it says no.
Use agents you trust, as you would in a terminal.

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

## Checking by hand

`cargo test -p wordcraft-assist acp` and `cargo test -p wordcraft-cli --test it acp` run the
scripted fake agent (`crates/assist/src/acp/fake_agent.rs`) through all of this, as Claude Code,
Codex, Pi and a custom agent would; `cargo test -p wordcraft-cli --test it call` tries
`wordcraft-cli call`. With a real agent installed:

1. Start WordCraft, open a document and the assistant pane, and pick the agent.
2. Ask it to rewrite a sentence: the rewrite appears as a tracked change in the agent's name.
3. Ask it to save the document somewhere, or to read a file: it says it can't.
4. End the conversation: the agent's processes are gone, and so is its `wordcraft-agent-…`
   folder.
