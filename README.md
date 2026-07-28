# A2Web — Web App Agent System

**A2Web** lets you launch web applications in native windows and connect them to AI agents. Web apps register tools and send observations via the [WebMCP](https://webmachinelearning.github.io/webmcp/) polyfill. Sub-agents (created via pi's SDK) receive observations automatically and proactively invoke tools.

## How It Works

```
Pi Agent ──JSON WS──► Harness ──CRDT──► Web Host ──wry──► Webviews
   │                    │                          (native windows)
   │ observations       │
   └── forwarded ───────┘
          │
          ▼
     Sub-Agent (pi SDK session)
     - Receives observations via followUp prompts
     - Invokes registered web app tools proactively
```

- **Pi Extension** (`.pi/extensions/a2web/`) — registers tools (`start_sub_agent`, `launch_webview`, `invoke_webapp_tool`), creates sub-agent sessions, forwards observations
- **Harness** (`harness/`) — Rust binary bridging pi ↔ web-host via JSON WebSocket (port 2341) and samod CRDT sync (port 2342)
- **Web Host** (`web-host/`) — Rust binary using wry + tao for native webview windows, injects the WebMCP polyfill and bridge JS
- **WebMCP Polyfill** (`web-host/src/webmcp.js`) — implements `document.modelContext` with `registerTool`, `sendObservation`, `getTools`

## Quick Start

```bash
# 1. Build Rust binaries
cargo build -p shared -p harness -p web-host

# 2. Install extension dependencies
cd .pi/extensions/a2web && npm install && cd ../..

# 3. Run pi in this directory
pi

# 4. In pi, create a sub-agent and launch apps
#    (see AGENTS.md for examples)
```

## Example

Create a counter app and a todo list, both linked to the same sub-agent:

```
/reload
start_sub_agent session_id="demo-session"
launch_webview app_id="counter" html="..." session_id="demo-session"
launch_webview app_id="todos" html="..." session_id="demo-session"
```

The sub-agent sees observations from both apps. Add a todo like "increment counter to 10" — the sub-agent will call `increment` until the counter reaches 10, then mark the todo as done.

## Project Structure

```
a2web/
  AGENTS.md                  # Architecture, tools, data flow, patterns
  Cargo.toml                 # Rust workspace
  shared/src/lib.rs          # CRDT document types (AgentDoc)
  harness/src/main.rs        # Bridge: pi JSON WS + samod CRDT server
  web-host/
    src/main.rs              # wry/tao webview host
    src/webmcp.js            # WebMCP polyfill injected into webviews
  .pi/extensions/a2web/
    index.ts                 # Extension entry: registers tools, lifecycle
    tools.ts                 # Tool implementations + sub-agent system
    types.ts                 # TypeScript types for harness messages
    doc-bridge.ts            # WebSocket connection to harness
    harness.ts               # Harness process management
```

## License

MIT
