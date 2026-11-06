# The writing assistant

WordCraft's assistant drafts, rewrites, summarizes and edits the open document in a conversation
in the assistant pane. Whichever backend runs it, it is a **participant** of the shared document:
it joins the document's room with an invite of its own, under its backend's name ("Claude Code",
"Codex"…), and acts only through the commands the [MCP server](mcp.md) offers any agent. So what
it may do is what its role lets a participant do, and every change it makes carries its name.

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
| `acp:custom` | Custom agent | any agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com) on stdio |

The pane lists them with what each needs first; `assist.backend {"id": "acp:claude-code"}` picks
one from a script. Each conversation gets the backend picked when it starts.

An ACP agent is a program on your computer that WordCraft starts for the conversation (on its
first turn: the pane shows "Start Claude Code" while it starts) and speaks the Agent Client
Protocol with over its standard input and output. The agent runs its own model loop with its own
account; WordCraft gives it the document through its MCP server and shows what it says, thinks,
plans and does in the pane.

### Setting up an agent

WordCraft never installs, downloads or bundles an agent. It looks for each preset's adapter on
`PATH` (without a shell), and when the adapter isn't installed but Node.js is, runs it with
`npx -y <package>`. When neither is there, the pane says what to install.

- **Claude Code.** `npm install -g @agentclientprotocol/claude-agent-acp`, or have Node.js on
  `PATH` for npx. Sign in once by running `claude` in a terminal.
- **Codex.** `npm install -g @agentclientprotocol/codex-acp`, or have Node.js on `PATH` for npx.
  Sign in once with `codex login` in a terminal, or give it an API key (`CODEX_API_KEY` or
  `OPENAI_API_KEY`) in its environment below.
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

A preset's `command` and `args` replace its adapter; `env` adds to the environment the agent
starts with. A damaged file is read for what it holds; anything that isn't what it should be is
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
- It starts in a new empty folder (`wordcraft-agent-…` in the temporary folder, only you may
  open it), with WordCraft's environment minus WordCraft's own variables, plus the agent's
  settings. WordCraft offers it no file access and no terminal through the protocol.
- When the conversation ends, or the window closes, WordCraft ends the agent and everything it
  started (on Unix, its whole process group), removes the folder, and takes the invite back:
  the key opens nothing from then on. Stop ends a turn; an agent that doesn't stop within 10
  seconds is ended, and starts again on the next turn.

The agent itself is a program you run, with your permissions. Its own tools may read files or
run commands; an ACP agent asks first, and the pane shows you the question. A conversation
started from a script (control channel, MCP, command line) never asks: it says no. Use agents
you trust, as you would in a terminal.

What the agent sends its model, and where, is its business: Claude Code talks to Anthropic and
Codex to OpenAI, under your account with them. Each turn WordCraft gives it your prompt and the
selection with the paragraphs around it and the outline, held to a few kilobytes; the first turn
adds a note that says which document it works on, its name and role, and how to use the
`wordcraft` tools. Through those tools it can read the whole shared document.

## Checking by hand

`cargo test -p wordcraft-assist acp` and `cargo test -p wordcraft-cli --test it acp` run the
scripted fake agent (`crates/assist/src/acp/fake_agent.rs`) through all of this. With a real
agent installed:

1. Start WordCraft, open a document and the assistant pane, and pick the agent.
2. Ask it to rewrite a sentence: the rewrite appears as a tracked change in the agent's name.
3. Ask it to save the document somewhere, or to read a file: it says it can't.
4. End the conversation: the agent's processes are gone, and so is its `wordcraft-agent-…`
   folder.
