# A2Web — Web App Agent System

Pi launches web apps in native webviews. Apps register tools and send observations via the [WebMCP](https://webmachinelearning.github.io/webmcp/) polyfill. Observations are forwarded to sub-agents that proactively act on them.

## Quick Start

```bash
# 1. Build Rust binaries
cargo build -p shared -p harness -p web-host

# 2. Install extension deps
cd .pi/extensions/a2web && npm install && cd ../..

# 3. Run pi in this directory
pi
```

## Architecture

```
┌──────────────┐    JSON WS      ┌──────────┐    samod CRDT    ┌──────────┐   wry    ┌───────────┐
│  Pi Agent    │◄──────────────►│  Harness  │◄──────────────►│ Web Host │◄───────►│ Webviews  │
│  + Extension │   port 2341    │  (Rust)  │   port 2342    │  (Rust)  │         │ (w/WebMCP)│
└──────┬───────┘                └──────────┘                └──────────┘         └───────────┘
       │
       │ observations forwarded
       ▼
┌──────────────┐
│ Sub-Agent    │  (pi SDK AgentSession)
│ session      │  proactively invokes tools
└──────────────┘
```

### Components

**Pi Extension** (`.pi/extensions/a2web/`)
- Registers `start_sub_agent`, `launch_webview`, `invoke_webapp_tool`
- Creates sub-agent sessions via `createAgentSession()` from pi SDK
- Forwards observations from webviews to linked sub-agents
- Cleans up sessions and processes on shutdown

**Harness** (`harness/`)
- Rust binary bridging pi ↔ web-host
- JSON WebSocket server (port 2341) for pi extension
- samod CRDT WebSocket server (port 2342) for web-host sync
- Bridge loop: watches CRDT changes, forwards new observations to pi

**Web Host** (`web-host/`)
- Rust binary using wry + tao for native webview windows
- Injects WebMCP polyfill + bridge JS into every webview
- Handles IPC from webview JS: tool registration, observations, tool results
- Writes all data to the shared CRDT document

**WebMCP Polyfill** (`web-host/src/webmcp.js`)
- Implements `document.modelContext` per the WebMCP spec
- `registerTool({name, description, inputSchema, execute})` — registers a tool
- `sendObservation(data, label)` — sends observation to the agent
- `getTools()` — lists registered tools
- Tool results are sent back as `tool_result` observations

## Tools

| Tool | Description |
|------|-------------|
| `start_sub_agent` | Creates a sub-agent session with `invoke_webapp_tool`. Returns `session_id`. |
| `launch_webview` | Launches a web app in a native webview. Optional `session_id` links it to a sub-agent. |
| `invoke_webapp_tool` | Calls a tool registered by a web app. Provide `app_id`, `tool_name`, `arguments`. |

## Sub-Agent

Created with `start_sub_agent`. The sub-agent:
- Has `invoke_webapp_tool` registered to call tools on webviews
- Receives observations automatically via `session.prompt()` with `followUp` behavior
- Is told to act proactively: on receiving an observation, assess it and invoke tools as needed
- Calls tools ONE AT A TIME, waiting for the result observation before proceeding

## Data Flow

```
1. App loads → registerTool() → ipc.postMessage('tools') 
   → web-host writes to CRDT → tools_available observation emitted

2. User interacts → sendObservation() → ipc.postMessage('obs')
   → web-host writes to CRDT → samod sync → harness
   → JSON WS → pi extension → forwardObservationToSubAgent()
   → sub-agent session.prompt()

3. Sub-agent decides → invoke_webapp_tool({app_id, tool_name, args})
   → extension sends to harness → CRDT → web-host
   → evaluate_script() → polyfill._executeTool() 
   → result sent back as tool_result observation

4. Tool result observation → forwarded to sub-agent (same path)
   → sub-agent sees result, decides next action
```

## Session Cleanup

On `session_shutdown`:
1. Send `exit` message to harness
2. `disposeAllSessions()` — disposes all sub-agent sessions
3. `stopHarness()` — kills harness + orphan web-host processes

## Ports

| Port | Protocol | Purpose |
|------|----------|---------|
| 2341 | JSON WebSocket | Pi extension ↔ Harness |
| 2342 | samod CRDT WebSocket | Harness ↔ Web Host |

## Project Structure

```
a2web/
  AGENTS.md
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

## Example Session

```
1. /reload
2. start_sub_agent session_id="demo-session"
3. launch_webview app_id="counter" html="..." session_id="demo-session"
4. launch_webview app_id="todos" html="..." session_id="demo-session"
5. [User adds "increment to 10" to todos]
6. Sub-agent sees todos_update observation, calls increment repeatedly
7. Counter reaches 10, sub-agent stops
```

## WebMCP Reference

Based on [WebMCP spec](https://webmachinelearning.github.io/webmcp/) (2026-07-21):

- `document.modelContext` — exposed on every webview's Document
- `registerTool(tool, options?)` — registers a tool with name, description, inputSchema, execute
- `getTools(options?)` — lists registered tools from this frame tree
- `sendObservation(data, label?)` — A2Web extension: sends observation to the agent
- `ontoolchange` event — fired when tools are added/removed

## Build

```bash
cargo build -p shared -p harness -p web-host
```

Only the Rust crates need building. The extension is TypeScript, loaded directly by pi.
