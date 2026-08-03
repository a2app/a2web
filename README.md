# A2Web — Web App Intent System

**A2Web** lets you launch web applications in native windows and connect them to AI agents. Web apps register **intents** — typed, schematized data they make available (`{type, id}`, no content). Registrations are announced to the main agent's context, and the agent can read, write, and transfer intent content between apps through dedicated tools — without the content ever passing through the agent's context during transfers.

## How It Works

```
Pi Agent ──JSON WS──► Harness ──CRDT──► Web Host ──wry──► Webviews
   │                                        │          (native windows)
   │ intent_registered / intent_response    │
   └── injected into main agent context ────┘
```

- **Pi Extension** (`.pi/extensions/a2web/`) — registers tools (`launch_webview`, `get_intent`, `set_intent`, `send_intent_to`), injects intent registration announcements into the main agent's context
- **Harness** (`harness/`) — Rust binary bridging pi ↔ web-host via JSON WebSocket (port 2341) and samod CRDT sync (port 2342)
- **Web Host** (`web-host/`) — Rust binary using wry + tao for native webview windows, injects the WebMCP polyfill and bridge JS, routes intent ops between apps
- **WebMCP Polyfill** (`web-host/src/webmcp.js`) — implements `document.modelContext` with `registerIntent`, `unregisterIntent`, `sendIntent`, `getIntent`, `setIntent`

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

Launch a source app that holds a note and an inbox app that receives it:

```
/reload
launch_webview app_id="notes-source" html="<textarea id='note'></textarea><script>document.modelContext.registerIntent('notes','note-1');</script>"
launch_webview app_id="notes-inbox" html="<script>document.modelContext.registerIntent('notes','inbox');</script>"
```

The agent's context receives `[A2Web] Intent registered — app: notes-source, type: notes, id: note-1` and the same for the inbox. Ask the agent to "transfer the note to the inbox" — the content moves directly between the two apps, the agent only sees a new registration announcement for the cloned intent in the inbox.

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
    src/main.rs              # wry/tao webview host + intent op routing
    src/webmcp.js            # WebMCP polyfill injected into webviews
  .pi/extensions/a2web/
    index.ts                 # Extension entry: tools, lifecycle, context injection
    tools.ts                 # Tool implementations + intent registry
    types.ts                 # TypeScript types for harness messages
    doc-bridge.ts            # WebSocket connection to harness
    harness.ts               # Harness process management
```

## License

MIT
