# A2Web — Web App Intent & Entity System

Pi launches web apps in native webviews. Apps communicate through **intents** and **entities**:
an **intent** is a *capability* an app advertises ("this app handles notes"), and an **entity**
is a specific thing linked to that intent (each note is a separate entity). Only types and ids
are injected into the **main agent's** context; content stays app-side and is read/written/moved
through dedicated tools.

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
       │ announcement / entity_response
       ▼
  main agent context (pi.sendMessage, followUp + triggerTurn)
```

### Components

**Pi Extension** (`.pi/extensions/a2web/`)
- Registers `launch_webview`, `get_entities_for_intent`, `set_entity`, `send_entity_to`
- On `announcement` messages from the harness (intent/entity registered or unregistered), injects
  `[A2Web] Intent registered — app: <app>, type: <type>` /
  `[A2Web] Entity registered — app: <app>, intent: <type>, id: <id>` into the **main agent's**
  context via `pi.sendMessage(..., { deliverAs: "followUp", triggerTurn: true })`
- Keeps a mirror registry (`app_id → {intents, entities}`) for fail-fast validation of tool calls
- Live footer status (`ctx.ui.setStatus`) showing app/intent/entity counts; resets when the
  harness dies and restarts

**Harness** (`harness/`)
- Rust binary bridging pi ↔ web-host: JSON WebSocket (port 2341) + samod CRDT sync (port 2342)
- Writes `entity_ops` into the CRDT when pi calls a tool; forwards new `announcements` and
  `entity_responses` back to pi

**Web Host** (`web-host/`)
- Rust binary using wry + tao for native webview windows; injects the WebMCP polyfill + bridge JS
- Owns the registries (`a.intents` capabilities, `a.entities` instances) and validates every op
- Fires `entity-request` / `entity-receive` events into webviews; relays transfer content
  app-to-app **without it ever entering the agent context**

**WebMCP Polyfill** (`web-host/src/webmcp.js`)
- `document.modelContext.registerIntent(type)` / `unregisterIntent(type)` — capability ("I handle notes")
- `registerEntity(type, id)` / `unregisterEntity(type, id)` — a specific note (requires the intent)
- `sendEntity(type, id, data, requestId)` / `sendEntities(type, [{id, data}], requestId)` — app's
  responses to `entity-request` events
- `getEntity(type, id)` / `setEntity(type, id, data)` — app-local content store (never announced;
  `setEntity` on a new entity registers it, announcements deduped system-side)
- `getEntities(type)` — app-local entity ids for a type
- Events the system fires into the app:
  - `entity-request` `{requestId, type, id, mode}` — `mode: "list"` (app answers with `sendEntities`),
    `"read"` or `"transfer"` (app answers with `sendEntity`; on transfer the app may delete its copy)
  - `entity-receive` `{type, id, data, source}` — content delivered (`source: "agent" | "transfer"`)

## Intent Schemes

Only one intent type exists today: **`notes`** = `{ id: string, title: string, content: string }`.
- Capability registration carries only the type; entity registration only `{type, id}` — never content.
- Entity content is validated/normalized against the scheme (id field filled automatically).
- `send_entity_to` clones an entity between apps that handle the **same** intent type; the clone
  keeps the source's id and is announced as a new entity registration in the target app.
- Whether the source deletes its copy after a transfer is the app's choice (it sees
  `mode: "transfer"` on the request event).

## Tools

| Tool | Description |
|------|-------------|
| `launch_webview` | Launches a web app in a native webview. |
| `get_entities_for_intent` | Lists the entities (with content) an app holds for an intent type (`app`, `intent_type`). Only when the user asks to see what's in an app. |
| `set_entity` | Writes content into a specific entity (`app`, `intent_type`, `id`, `content`). Only when the user asks to write. |
| `send_entity_to` | Transfers an entity app-to-app (`src_app`, `intent_type`, `id`, `target_app`). Content never shown to the agent. |

## Data Flow

```
1. App loads → registerIntent('notes') → ipc.postMessage('intent')
   → web-host validates + writes CRDT (intents + announcement)
   → harness forwards announcement → extension injects into main agent context

2. App registers a note → registerEntity('notes', id) → ipc.postMessage('entity')
   → web-host validates (intent must exist) + writes CRDT (entities + announcement)

3. Agent calls get_entities_for_intent / set_entity / send_entity_to
   → extension sends entity_op → harness writes CRDT (entity_ops)
   → web-host validates against registries, fires 'entity-request' into the source app
   → app responds: sendEntity / sendEntities → ipc.postMessage('entity-content')

4. list:  web-host writes entity_response {kind: list, data: [entities]} → extension resolves tool
   read:  web-host writes entity_response {kind: read, data: entity}
   set:   web-host fires 'entity-receive' {source: agent} into the app → writes response
   transfer: web-host fires 'entity-receive' {source: transfer} into the target app
            → writes response + an entity_registered announcement for the target (same id)

5. entity_response → forwarded to pi → extension resolves the awaiting tool call
```

## Session Cleanup

On `session_shutdown`:
1. Send `exit` message to harness
2. `disposeExtensionState()` — clears the registry
3. `stopHarness()` — kills harness + orphan web-host processes

If the harness is killed externally (e.g. `killall harness web-host`), the next tool call detects
the dead connection, resets the registry/footer, and restarts the harness with a fresh doc.

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
    src/main.rs              # wry/tao webview host + entity op routing
    src/webmcp.js            # WebMCP polyfill injected into webviews
  .pi/extensions/a2web/
    index.ts                 # Extension entry: tools, lifecycle, context injection, footer status
    tools.ts                 # Tool implementations + intent/entity registry
    types.ts                 # TypeScript types for harness messages
    doc-bridge.ts            # WebSocket connection to harness
    harness.ts               # Harness process management
```

## Example Session

```
1. /reload
2. launch_webview app_id="notes-source" html="<textarea id='note'>...</textarea><script>...registerIntent('notes'); registerEntity('notes','note-1')...</script>"
3. launch_webview app_id="draft-email" html="...registerIntent('notes')..."
4. [Agent context gets: Intent registered — app: notes-source, type: notes]
   [Agent context gets: Entity registered — app: notes-source, intent: notes, id: note-1]
5. User asks: "transfer the note to draft email"
6. Agent calls send_entity_to({src_app: notes-source, intent_type: notes, id: note-1, target_app: draft-email})
7. Content moves app-to-app; context gets: Entity unregistered — app: notes-source, ... note-1
   and: Entity registered — app: draft-email, intent: notes, id: note-1
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
let scan = || { /* launch pending webviews + dispatch entity ops */ };
scan();  // initial scan — critical
while let Some(_) = changes.next().await { scan(); }
```
The `pending_launches` / `pending_ops` guards make the rescan idempotent.

### 3. Where to Put Behavior Constraints: Agent Addendum vs Tool Descriptions

The main-agent addendum (`agent-addendum.md`) stays **generic** — operational rules only:
- Track intent capabilities and entities from the announcements in context
- Only access/move entity content when the user actually asks
- Don't peek at content before a transfer — `send_entity_to` handles it without exposure

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
- The extension's registry/footer must be reset when the harness restarts (see `ensureConnected`)

### 6. Process Cleanup

```bash
# Full cleanup before restart
killall -9 harness web-host
# Also free ports
lsof -ti:2341 | xargs kill -9
lsof -ti:2342 | xargs kill -9
```

`pkill -f` is unreliable because it may miss some process paths. Use `killall` by exact
binary name. Always verify with `ps` and `lsof`. Never target pi itself.

### 7. Entity Op Correlation & Validation

- Every entity op gets a unique `op_id` from the extension; the web-host echoes it through
  the `entity-request` event's `requestId` and the app's response, so responses always
  correlate to the right awaiting tool call.
- The web-host is the **authority** for validation (is the intent registered? does the target
  app handle the same type?). The extension's registry only fail-fasts on obvious mistakes;
  real validation errors come back as `is_error` responses.
- Dedupe announcements: an intent/entity is announced the first time it appears in an app;
  repeat registrations (e.g. `setEntity` re-announcing a transferred clone) are silent
  (`push_announcement` checks the registries first).

### 8. Observation-Style Fields Grow Forever

`announcements` and `entity_responses` are never cleared; the harness tracks the
last-forwarded index (like observations before them). Fine for development.
`entity_ops` are the exception — the web-host clears them after dispatching.
