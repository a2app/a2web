# A2Web — Web App Intent System

Pi launches web apps in native webviews. Apps communicate through **intents** — typed, schematized data they register as available (`registerIntent(type, id)`). Intent registrations (type + id only) are injected into the **main agent's** context; content stays app-side and is read/written/moved through dedicated tools.

## Quick Start

```bash
# 1. Build Rust binaries
cargo build -p shared -p harness -p web-host

# 2. Install extension deps
cd .pi/extensions/a2web && npm install && cd ../..

# 3. Run pi in this directory
pi

# 4. Launch apps and transfer notes between them
```

## Architecture

```
┌──────────────┐    JSON WS      ┌──────────┐    samod CRDT    ┌──────────┐   wry    ┌───────────┐
│  Pi Agent    │◄──────────────►│  Harness  │◄──────────────►│ Web Host │◄───────►│ Webviews  │
│  + Extension │   port 2341    │  (Rust)  │   port 2342    │  (Rust)  │         │ (w/WebMCP)│
└──────────────┘                └──────────┘                └──────────┘         └───────────┘
       │
       │ intent_registered / intent_response
       ▼
  main agent context (pi.sendMessage, followUp + triggerTurn)
```

### Components

**Pi Extension** (`.pi/extensions/a2web/`)
- Registers `launch_webview`, `get_intent`, `set_intent`, `send_intent_to`
- On `intent_registered` / `intent_unregistered` messages from the harness, injects
  `[A2Web] Intent registered — app: <app>, type: <type>, id: <id>` into the **main agent's**
  context via `pi.sendMessage(..., { deliverAs: "followUp", triggerTurn: true })`
- Keeps a mirror registry (`app_id → intent_id → type`) for fail-fast validation of tool calls

**Harness** (`harness/`)
- Rust binary bridging pi ↔ web-host: JSON WebSocket (port 2341) + samod CRDT sync (port 2342)
- Writes `intent_ops` into the CRDT when pi calls a tool; forwards new
  `intent_registrations` / `intent_unregistrations` / `intent_responses` back to pi

**Web Host** (`web-host/`)
- Rust binary using wry + tao for native webview windows; injects the WebMCP polyfill + bridge JS
- Owns the intent **registry** (`a.intents`) and validates every op against it
- Fires `intent-request` / `intent-receive` events into webviews; relays transfer content
  app-to-app **without it ever entering the agent context**

**WebMCP Polyfill** (`web-host/src/webmcp.js`)
- `document.modelContext.registerIntent(type, id)` / `unregisterIntent(type, id)` — announces
  `{type, id}` to the system; validates against the known intent schemes (only `notes` for now)
- `sendIntent(type, id, data, requestId)` — app's response to an `intent-request` event
- `getIntent(type, id)` / `setIntent(type, id, data)` — app-local content store (never announced)
- Events the system fires into the app:
  - `intent-request` `{requestId, type, id}` — app must respond via `sendIntent`
  - `intent-receive` `{type, id, data, source}` — system delivers content (`source: "agent" | "transfer"`)

## Intent Schemes

Only one intent type exists today: **`notes`** = `{ id: string, title: string, content: string }`.
- Registration only carries `{type, id}` — never content.
- Content is validated/normalized against the scheme (id field filled automatically).
- `send_intent_to` clones content between apps of the **same type**; the cloned intent keeps
  the source's id and is announced as a new registration in the target app.

## Tools

| Tool | Description |
|------|-------------|
| `launch_webview` | Launches a web app in a native webview. |
| `get_intent` | Reads an intent's content (`app`, `id`). Only when the user asks to read. |
| `set_intent` | Writes content into an intent (`app`, `id`, `content`). Only when the user asks to write. |
| `send_intent_to` | Clones content app-to-app (`src_app`, `id`, `target_app`). Content never shown to the agent. |

## Data Flow

```
1. App loads → registerIntent('notes', id) → ipc.postMessage('intent')
   → web-host validates + writes CRDT (intents + intent_registrations)
   → harness forwards intent_registered → extension injects into main agent context

2. Agent calls get_intent / send_intent_to
   → extension sends intent_op → harness writes CRDT (intent_ops)
   → web-host validates against registry, fires 'intent-request' into the source app
   → app responds: sendIntent(type, id, data, requestId) → ipc.postMessage('intent-content')

3. get:  web-host writes intent_response {kind: get, data} → harness → extension resolves tool
   set:  web-host fires 'intent-receive' {source: agent} into the app → writes response
   transfer: web-host fires 'intent-receive' {source: transfer} into the target app
            → writes response + a new intent_registration for the target (same id)

4. intent_response → forwarded to pi → extension resolves the awaiting tool call
```

## Session Cleanup

On `session_shutdown`:
1. Send `exit` message to harness
2. `disposeExtensionState()` — clears the registry
3. `stopHarness()` — kills harness + orphan web-host processes

## Ports

| Port | Protocol | Purpose |
|------|----------|---------|
| 2341 | JSON WebSocket | Pi extension ↔ Harness |
| 2342 | samod CRDT WebSocket | Harness ↔ Web Host |

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

## Example Session

```
1. /reload
2. launch_webview app_id="notes-source" html="<textarea id='note'>...</textarea><script>...registerIntent('notes','note-1')...</script>"
3. launch_webview app_id="notes-inbox" html="...registerIntent('notes','inbox')..."
4. [Agent context gets: Intent registered — app: notes-source, type: notes, id: note-1]
5. User asks: "transfer the note to the inbox"
6. Agent calls send_intent_to({src_app: notes-source, id: note-1, target_app: notes-inbox})
7. Content moves app-to-app; context gets: Intent registered — app: notes-inbox, type: notes, id: note-1
```

## Build

```bash
cargo build -p shared -p harness -p web-host
```

Only the Rust crates need building. The extension is TypeScript, loaded directly by pi.

---

## Lessons Learned & Best Patterns

### 1. HTML Content Must Be Body-Only

The web-host wraps app HTML inside a full document template:
```rust
.with_html(&format!(
    r#"...<body>{}</body>..."#, html
))
```
**Always pass only body content** — no `<html>`, `<head>`, or `<body>` tags. The polyfill and bridge scripts are already injected before your content.

### 2. CRDT Change Stream Only Emits Future Changes

`DocHandle::changes()` in samod does **not** replay the current document state. If an op or
launch lands in the CRDT before the web-host subscribes to changes, it is silently missed —
the web-host never reads the doc again. This manifests as "app never launches" or "tool
hangs" when the extension starts the harness and immediately launches apps.

**Fix:** scan the current document state once after connecting, then rescan on every change:
```rust
let mut changes = handle.changes();
let scan = || { /* launch pending webviews + dispatch intent ops */ };
scan();  // initial scan — critical
while let Some(_) = changes.next().await { scan(); }
```
The `pending_launches` / `pending_ops` guards make the rescan idempotent.

### 3. Where to Put Behavior Constraints: Agent Addendum vs Tool Descriptions

The main-agent addendum (`agent-addendum.md`) stays **generic** — operational rules only:
- Track intents from the `Intent registered` messages in context
- Only access/move intent content when the user actually asks
- Don't peek at content before a transfer — `send_intent_to` handles it without exposure

App-specific behavior belongs in each app's event handlers and in what the app stores
app-side. The agent's tools are the only interface; their descriptions carry the
when-to-use rules (get/set only on explicit ask, transfer never exposes content).

### 4. Extension Changes Require Pi Reload

The extension TypeScript is loaded by pi at startup via `tsx`. Unlike the Rust binaries,
changes to `.ts` files in `.pi/extensions/a2web/` require **`/reload`** in pi to take effect.
Rust binaries are rebuilt fresh each time the harness spawns the web-host.

### 5. CRDT Uses InMemoryStorage by Default

`samod::Repo::build_tokio()` uses `InMemoryStorage` — no filesystem persistence. Data is
lost when the harness process dies. This is fine for development but means:
- Killing and restarting the harness gives a **fresh document**
- Old web-host processes (from before the restart) become orphaned with stale data connections
- Always `killall -9 harness web-host` before restarting to avoid orphan windows

### 6. Process Cleanup

```bash
# Full cleanup before restart
killall -9 harness web-host
# Also free ports
lsof -ti:2341 | xargs kill -9
lsof -ti:2342 | xargs kill -9
```

`pkill -f` is unreliable because it may miss some process paths. Use `killall` by exact
binary name. Always verify with `ps` and `lsof`.

### 7. Intent Op Correlation & Validation

- Every intent op gets a unique `op_id` from the extension; the web-host echoes it through
  the `intent-request` event's `requestId` and the app's `sendIntent` response, so responses
  always correlate to the right awaiting tool call.
- The web-host is the **authority** for validation (is the intent registered? does the
  target app have the same type?). The extension's registry only fail-fasts on obvious
  mistakes; real validation errors come back as `is_error` responses.
- Dedupe registration announcements: an intent is announced to the agent the first time it
  appears in an app; repeat transfers of the same `(app, type, id)` just refresh content
  silently (`push_registration` checks `a.intents` first).

### 8. Observation-Style Fields Grow Forever

`intent_registrations`, `intent_unregistrations`, and `intent_responses` are never cleared;
the harness tracks the last-forwarded index (like observations before them). Fine for
development. `intent_ops` are the exception — the web-host clears them after dispatching.
