import { Type } from "typebox";
import { connectToHarness, sendToHarness, onMessage } from "./doc-bridge.js";
import { startHarness } from "./harness.js";
import type { HarnessMessage, EntityOp } from "./types.js";

type ExtensionAPI = any;

let harnessStarted = false;
let harnessReady: Promise<void> | null = null;

// ── Intent/entity registry ──────────────────────────────────────────────
// Mirrors what the agent has been told about (capabilities and entity ids
// only). Used to fail fast on obviously invalid tool calls; the web-host is
// the final authority and returns proper errors through entity_response.

interface AppRegistry {
  intents: Set<string>;
  entities: Map<string, string>; // entity_id -> intent_type
}

const registry = new Map<string, AppRegistry>();

function appReg(appId: string): AppRegistry {
  let r = registry.get(appId);
  if (!r) {
    r = { intents: new Set(), entities: new Map() };
    registry.set(appId, r);
  }
  return r;
}

export function registerIntentInRegistry(appId: string, intentType: string): void {
  appReg(appId).intents.add(intentType);
}

export function unregisterIntentInRegistry(appId: string, intentType: string): void {
  const r = registry.get(appId);
  if (!r) return;
  r.intents.delete(intentType);
  for (const [id, t] of Array.from(r.entities.entries())) {
    if (t === intentType) r.entities.delete(id);
  }
  if (r.intents.size === 0 && r.entities.size === 0) registry.delete(appId);
}

export function registerEntityInRegistry(appId: string, intentType: string, entityId: string): void {
  appReg(appId).entities.set(entityId, intentType);
}

export function unregisterEntityInRegistry(appId: string, entityId: string): void {
  const r = registry.get(appId);
  if (!r) return;
  r.entities.delete(entityId);
  if (r.intents.size === 0 && r.entities.size === 0) registry.delete(appId);
}

function knownEntity(appId: string, entityId: string): string | undefined {
  return registry.get(appId)?.entities.get(entityId);
}

function appHandlesIntent(appId: string, intentType: string): boolean {
  return registry.get(appId)?.intents.has(intentType) ?? false;
}

// ── Harness lifecycle ───────────────────────────────────────────────────

export async function ensureConnected(): Promise<void> {
  if (harnessStarted) {
    const { quickConnectCheck } = await import("./doc-bridge.js");
    if (await quickConnectCheck()) return;
    // The harness died (e.g. killed externally) — its doc is fresh, so all
    // tracked apps/intents/entities are stale. Reset and reconnect.
    harnessStarted = false;
    harnessReady = null;
    disposeExtensionState();
    statusCb?.("A2Web: reconnecting…");
  }
  if (!harnessReady) {
    harnessReady = (async () => {
      startHarness(process.cwd());
      await connectToHarness();
      harnessStarted = true;
    })();
  }
  await harnessReady;
  statusCb?.(statsText());
}

export function disposeExtensionState(): void {
  registry.clear();
}

export function getRegistryStats(): { apps: number; intents: number; entities: number } {
  let intents = 0;
  let entities = 0;
  for (const r of registry.values()) {
    intents += r.intents.size;
    entities += r.entities.size;
  }
  return { apps: registry.size, intents, entities };
}

export function statsText(): string {
  const s = getRegistryStats();
  return `A2Web: ${s.apps} app${s.apps === 1 ? "" : "s"} · ${s.intents} intent${s.intents === 1 ? "" : "s"} · ${s.entities} entit${s.entities === 1 ? "y" : "ies"}`;
}

// The extension entry point wires this to the TUI footer so the status can
// reflect harness restarts (e.g. the harness being killed externally).
let statusCb: ((text: string) => void) | null = null;
export function setStatusCallback(cb: (text: string) => void): void {
  statusCb = cb;
}

// ── Entity op plumbing ──────────────────────────────────────────────────

function errResult(text: string) {
  return { content: [{ type: "text" as const, text }], details: {}, isError: true };
}
function okResult(text: string, details: Record<string, unknown> = {}) {
  return { content: [{ type: "text" as const, text }], details, isError: false };
}

type ToolResult = ReturnType<typeof okResult | typeof errResult>;

async function sendEntityOp(op: EntityOp): Promise<ToolResult> {
  try {
    await ensureConnected();
  } catch {
    return errResult("A2Web unavailable.");
  }
  const op_id = `op-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  sendToHarness({
    type: "entity_op",
    op_id,
    kind: op.kind,
    app_id: op.app_id,
    intent_type: op.intent_type,
    entity_id: op.entity_id,
    data: op.data ?? null,
    target_app: op.target_app ?? null,
  });

  return new Promise((resolve) => {
    const timeout = setTimeout(() => {
      resolve(
        errResult(
          `Timed out waiting for the entity response from app '${op.app_id}'. ` +
            `The app may not handle 'entity-request' events.`,
        ),
      );
    }, 20_000);

    const unsub = onMessage((m: HarnessMessage) => {
      if (m.type !== "entity_response" || m.op_id !== op_id) return;
      clearTimeout(timeout);
      unsub();
      if (m.is_error) {
        resolve(errResult(`Error: ${m.error ?? "unknown error"}`));
      } else if (op.kind === "list" && m.data) {
        resolve(okResult(m.data, { app_id: m.app_id, intent_type: m.intent_type }));
      } else if (op.kind === "read" && m.data) {
        resolve(okResult(m.data, { app_id: m.app_id, intent_type: m.intent_type, entity_id: m.entity_id }));
      } else {
        resolve(okResult(`Done.`, { app_id: m.app_id, intent_type: m.intent_type, entity_id: m.entity_id }));
      }
    });
  });
}

// ── Tool registration ───────────────────────────────────────────────────

export function registerTools(pi: ExtensionAPI): void {
  pi.registerTool({
    name: "launch_webview",
    label: "Launch WebView",
    description:
      "Launch a web app in a new native window (webview). The app HTML is fully rendered with the A2Web polyfill (document.modelContext.registerIntent / registerEntity etc.).",
    parameters: Type.Object({
      app_id: Type.String({ description: "Unique ID for this app window" }),
      html: Type.String({ description: "Full HTML document (body content is fine — the host wraps it)" }),
    }),
    async execute(_id: string, params: any) {
      try {
        await ensureConnected();
      } catch {
        return errResult("A2Web unavailable.");
      }
      sendToHarness({ type: "launch_webview", app_id: params.app_id, html: params.html });
      return okResult(`Launched '${params.app_id}'.`, { app_id: params.app_id });
    },
  });

  pi.registerTool({
    name: "get_entities_for_intent",
    label: "Get Entities for Intent",
    description:
      "List the entities a web app holds for an intent type (for 'notes': each note, with its content). Provide the app_id and the intent type. " +
      "The app must have registered that intent (you will have seen an '[A2Web] Intent registered' message in context, and 'Entity registered' messages for each note). " +
      "Only call this when the user explicitly asks to see what's in an app.",
    parameters: Type.Object({
      app: Type.String({ description: "app_id of the web app" }),
      intent_type: Type.String({ description: "intent type, e.g. 'notes'" }),
    }),
    async execute(_id: string, params: any) {
      if (registry.has(params.app) && !appHandlesIntent(params.app, params.intent_type)) {
        return errResult(`App '${params.app}' does not handle intent '${params.intent_type}'.`);
      }
      return sendEntityOp({
        kind: "list",
        app_id: params.app,
        intent_type: params.intent_type,
        entity_id: "",
      });
    },
  });

  pi.registerTool({
    name: "set_entity",
    label: "Set Entity Content",
    description:
      "Write content into a specific entity in a web app. Provide the app_id, the intent type, the entity id, and the new content matching the scheme " +
      "(for 'notes': {\"title\": ..., \"content\": ...} — the id field is filled in automatically). " +
      "Only call this when the user explicitly asks you to write or update entity content.",
    parameters: Type.Object({
      app: Type.String({ description: "app_id of the web app holding the entity" }),
      intent_type: Type.String({ description: "intent type, e.g. 'notes'" }),
      id: Type.String({ description: "entity id" }),
      content: Type.Object(
        {
          title: Type.String({ description: "Note title" }),
          content: Type.String({ description: "Note body text" }),
        },
        { additionalProperties: false },
      ),
    }),
    async execute(_id: string, params: any) {
      const type = knownEntity(params.app, params.id);
      if (type === undefined && registry.has(params.app)) {
        return errResult(`App '${params.app}' has no entity '${params.id}'.`);
      }
      return sendEntityOp({
        kind: "set",
        app_id: params.app,
        intent_type: type ?? params.intent_type,
        entity_id: params.id,
        data: JSON.stringify(params.content),
      });
    },
  });

  pi.registerTool({
    name: "send_entity_to",
    label: "Send Entity To",
    description:
      "Move (transfer) an entity from one web app (src_app) into another web app (target_app). Both apps must have registered the same intent type (e.g. both handle 'notes'). " +
      "The content is transferred directly between the two apps and is NEVER shown to you. " +
      "Whether the source app keeps or deletes its copy is the app's choice. " +
      "After a successful transfer a new '[A2Web] Entity registered' message for the target app (same id) appears in your context. " +
      "Only call this when the user asks to move or copy content between apps.",
    parameters: Type.Object({
      src_app: Type.String({ description: "app_id of the app that currently holds the entity" }),
      intent_type: Type.String({ description: "intent type, e.g. 'notes'" }),
      id: Type.String({ description: "entity id to transfer from src_app" }),
      target_app: Type.String({ description: "app_id of the app to receive the entity" }),
    }),
    async execute(_id: string, params: any) {
      const srcType = knownEntity(params.src_app, params.id);
      if (srcType === undefined && registry.has(params.src_app)) {
        return errResult(`App '${params.src_app}' has no entity '${params.id}'.`);
      }
      const intentType = srcType ?? params.intent_type;
      if (registry.has(params.target_app) && !appHandlesIntent(params.target_app, intentType)) {
        return errResult(
          `App '${params.target_app}' does not handle intent '${intentType}' — ` +
            `both apps must handle the same intent type for a transfer.`,
        );
      }
      return sendEntityOp({
        kind: "transfer",
        app_id: params.src_app,
        intent_type: intentType,
        entity_id: params.id,
        target_app: params.target_app,
      });
    },
  });
}
