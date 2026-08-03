# A2Web — Web App Intent & Entity System

**A2Web** lets you launch web applications in native windows and connect them to AI agents. Web apps advertise **intents** (capabilities: "this app handles notes") and register **entities** (specific instances: each note). Only types and ids are announced to the main agent's context; content is read, written, and transferred through dedicated tools — never exposed to the agent during transfers.

## How It Works

```
Pi Agent ──JSON WS──► Harness ──CRDT──► Web Host ──wry──► Webviews
   │                                        │          (native windows)
   │ announcement / entity_response         │
   └── injected into main agent context ────┘
```

- **Pi Extension** (`.pi/extensions/a2web/`) — registers tools (`launch_webview`, `get_entities_for_intent`, `set_entity`, `send_entity_to`), injects registration announcements into the main agent's context, live footer status
- **Harness** (`harness/`) — Rust binary bridging pi ↔ web-host via JSON WebSocket (port 2341) and samod CRDT sync (port 2342)
- **Web Host** (`web-host/`) — Rust binary using wry + tao for native webview windows, injects the WebMCP polyfill and bridge JS, routes entity ops between apps
- **WebMCP Polyfill** (`web-host/src/webmcp.js`) — implements `document.modelContext` with `registerIntent`, `registerEntity`, `sendEntity`, `sendEntities`, `getEntity`, `setEntity`

## Quick Start

```bash
# 1. Build Rust binaries
cargo build -p shared -p harness -p web-host

# 2. Install extension dependencies
cd .pi/extensions/a2web && npm install && cd ../..

# 3. Run pi in this directory
pi
```

## Example

Launch a source app that holds a note and a destination app that receives it:

```
/reload
launch_webview app_id="notes-source" html="<textarea id='note'></textarea><script>document.modelContext.registerIntent('notes'); document.modelContext.registerEntity('notes','note-1');</script>"
launch_webview app_id="draft-email" html="<script>document.modelContext.registerIntent('notes');</script>"
```

The agent's context receives `[A2Web] Intent registered — app: notes-source, type: notes`, `[A2Web] Entity registered — app: notes-source, intent: notes, id: note-1`, and the same for the destination. Ask the agent to "transfer the note to draft email" — the content moves directly between the two apps, the agent only sees the unregister/register announcements.

## Intent Types

Only one intent type is supported today:

| Type | Scheme |
|------|--------|
| `notes` | `{ id: string, title: string, content: string }` |

## Project Structure

```
a2web/
  AGENTS.md                  # Architecture, tools, data flow, patterns
  Cargo.toml                 # Rust workspace
  shared/src/lib.rs          # CRDT document types (AgentDoc) + intent scheme validation
  harness/src/main.rs        # Bridge: pi JSON WS + samod CRDT server
  web-host/
    src/main.rs              # wry/tao webview host + entity op routing
    src/webmcp.js            # WebMCP polyfill injected into webviews
  .pi/extensions/a2web/
    index.ts                 # Extension entry: tools, lifecycle, context injection, footer status
    tools.ts                 # Tool implementations + intent/entity registry
    types.ts                 # TypeScript types for harness messages
    doc-bridge.ts            # WebSocket connection to harness
    harness.ts               # Harness process management
```

## License

MIT
