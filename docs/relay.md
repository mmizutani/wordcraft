# Relay

> **Status:** first version, self-hosted relay (#76) · **Revisions:** 2026-11-07 first version

An AI agent on another computer, such as one in a cloud sandbox (E2B, a Kubernetes agent
sandbox, any cloud runner), reaches a document shared from someone's desktop through a relay. The
desktop app dials out to the relay over HTTPS and asks it, again and again, for the requests
agents sent. Nothing on the person's computer listens beyond it, so the relay works behind any
router or firewall that lets the computer browse the web. Nobody runs a tunnel.

```
agent (MCP client) ──HTTPS──▶ relay ◀──HTTPS── WordCraft app ──▶ the shared document
   https://relay.example.org/r/<room>/mcp        (dials out)
```

`wordcraft-relay` is one binary for anyone to run. There is no hosted relay yet: run your own,
or use one whose operator gives you a token.

## In the app

1. File › Share › Agents on other computers: enter the relay's address (`https://…`) and the
   token from whoever runs it, then Use this relay. Or, from the control channel or a script:
   `room.relay {"relay": "https://relay.example.org", "token": "…"}`.
2. Share the document. The app opens a room for it on the relay, and the page shows the URL
   agents use. While the relay can't be reached (the computer is offline, the relay is
   restarting), the app tries again, from every 2 seconds to every minute; a token the relay
   refuses isn't tried again.
3. Invite the agent with On another computer ticked (`room.invite` with `remote: true`). Its key
   shows once, with an MCP client configuration to give the agent:

   ```json
   {"mcpServers": {"wordcraft": {"type": "http", "url": "https://relay.example.org/r/<room>/mcp",
     "headers": {"Authorization": "Bearer <invite key>"}}}}
   ```

The agent then works on a copy of its own with the invite's role (a suggester's edits are tracked
changes in its name). Stop sharing and the room closes on the relay: agents get 404, the document
isn't shared any more. The app keeps the relay's address and token in its settings folder
(`relay.json`, readable only by the person), with the room it opened. Sharing again opens the same
room, so an agent's URL keeps working. Stop using this relay (`room.relay {"off": true}`) forgets
it.

## What the relay sees, and what it can't do

- **It checks no invite key and keeps none.** It passes each request to the app, which checks the
  key as it does for an agent on the same computer. A relay that is broken into can't let anyone
  into a document. It never gets the window's key either, which the app refuses from any agent.
- **It does see the requests and answers.** TLS ends at the relay, as at any tunnel service. An
  agent's requests and the document text in the answers pass through it. Whoever runs the relay
  is trusted with what the agents do, so run your own or use one you trust.
- **A room's URL is not a secret that lets anyone in.** Its 128 bits come from a hash of the
  room's secret, and each request still needs an invite's key. Only the desktop that opened a room
  holds its secret, which it needs to take requests and answer them, and to open the room again
  after the relay restarts: nobody else can take a room over by its URL.

## Running a relay

```sh
cargo build --release -p wordcraft-relay
openssl rand -hex 32 > tokens.txt     # one token per line, 32 characters or more
wordcraft-relay --public-url https://relay.example.org --tokens-file tokens.txt
```

| Option | |
|---|---|
| `--public-url URL` | the relay's address as agents and desktops reach it. `https://…`, or `http://127.0.0.1:PORT` to try one on one computer |
| `--listen ADDR` | where to listen, `127.0.0.1:8790` by default |
| `--tokens-file FILE` | the tokens desktops open rooms with, one per line (`#` starts a comment). Also `WORDCRAFT_RELAY_TOKENS`, comma-separated |
| `--trust-forwarded` | take agents' addresses from `X-Forwarded-For`, which the proxy in front sets, for the per-address limits |
| `--max-connections N` | connections served at once, 512 by default |

The relay speaks plain HTTP/1.1. Put a TLS proxy in front of it, on the public address. With
[Caddy](https://caddyserver.com), which gets a certificate on its own:

```
relay.example.org {
    reverse_proxy 127.0.0.1:8790 {
        flush_interval -1
        transport http {
            read_timeout 200s
        }
    }
}
```

Then run the relay with `--trust-forwarded`. The proxy has to let a request wait at least three
minutes for its answer: a desktop's poll waits up to 25 seconds for a request, and an agent's
request waits up to 180 seconds for the desktop's answer. `GET /healthz` answers `ok` for a health
check.

Keep the tokens as you would a password: a token lets anyone open rooms on the relay (up to 16 a
token), not reach documents. To take one back, remove it and restart the relay. The relay keeps
everything in memory. After a restart, desktops open their rooms again with the same URLs within
a poll or two; until then agents get 404 for the room. A room whose desktop is asleep or offline
answers 503 with `Retry-After`.

### Limits

| | |
|---|---|
| Rooms | 1,024 in all, 16 a token. A room whose desktop hasn't asked for 40 seconds is offline to agents; after 24 hours it is forgotten |
| An agent's request | 1 MiB, an answer within 180 seconds (else 504) |
| A desktop's answer | 96 MiB (a 32 MB PDF as base64). One larger than an agent's request is taken only while an agent waits, two at once from one token's desktops and 16 in all (else 413) |
| Answers waiting for agents to read them | 192 MiB from one token's rooms, 1 GiB in all: past that, new requests get 503 before they reach the desktop |
| Requests waiting for one desktop | 64 (else 503), and 128 in all the rooms of one token (else 503); a desktop may have 4 polls waiting at once (else 429) |
| Rate | 20 requests a second from one address (bursts of 40), 50 a second to one room (bursts of 100); more get 429 |
| A request's head and body | must arrive within 60 seconds of its start, and a connection may sit idle 60 seconds between requests |

## The protocol

Desktop to relay, each with the room's secret in `Authorization: Bearer`, except the first, which
carries a relay token:

| Request | Answer |
|---|---|
| `POST /host/rooms` `{}`, or `{"room", "secret"}` to open a room again (after the relay restarted) | `{"room", "secret", "url"}`; 401 for a wrong token, 409 for a room whose id isn't from that secret, or that another desktop is using |
| `POST /host/r/<room>/poll` | `{"requests": [{"id", "method", "target", "headers", "body", "ttlMs"}]}`, waiting up to 25 seconds for one; 404 once the relay has forgotten the room. `ttlMs` is how long the agent still waits: the app doesn't start a request after that, as its agent has been told it timed out, and its MCP endpoint is told what's left (`Wordcraft-Wait-Ms`) so it waits no longer in its own queue |
| `POST /host/r/<room>/reply` `{"id", "status", "headers", "body"}` | 204 |
| `DELETE /host/r/<room>` | closes the room |

Agents use `/r/<room>/mcp` like any MCP endpoint over Streamable HTTP
([MCP over HTTP](mcp.md#mcp-over-http)). The relay passes on only the headers MCP uses
(`Authorization`, `Content-Type`, `Accept`, `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name`,
`Mcp-Param-*`, `Mcp-Session-Id`, `Origin`, `Last-Event-ID`), and the answer's `Content-Type`,
`WWW-Authenticate`, `Allow`, `Retry-After` and `Mcp-Session-Id`. Bodies travel as JSON strings,
so they must be UTF-8 (MCP's are JSON): any other request body is refused with 400, and any other
answer becomes 502.
