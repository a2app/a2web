import { Type } from "typebox";
import { connectToHarness, sendToHarness, onMessage } from "./doc-bridge.js";
import { startHarness } from "./harness.js";
import type { HarnessMessage, IntentOp } from "./types.js";

type ExtensionAPI = any;

let harnessStarted = false;
let harnessReady: Promise<void> | null = null;

// ── Intent registry ─────────────────────────────────────────────────────
// Mirrors the intents the agent has been told about (type + id only). Used
// to fail fast on obviously invalid tool calls; the web-host is the final
// authority and returns proper errors through intent_response messages.

const intentRegistry = new Map<string, Map<string, string>>();

export function registerIntentInRegistry(appId: string, intentType: string, intentId: string): void {
  let byId = intentRegistry.get(appId);
  if (!byId) {
    byId = new Map();
    intentRegistry.set(appId, byId);
  }
  byId.set(intentId, intentType);
}

export function unregisterIntentInRegistry(appId: string, intentId: string): void {
  const byId = intentRegistry.get(appId);
  if (!byId) return;
  byId.delete(intentId);
  if (byId.size === 0) intentRegistry.delete(appId);
}

function knownIntent(appId: string, intentId: string): string | undefined {
  return intentRegistry.get(appId)?.get(intentId);
}

function appHasIntentType(appId: string, intentType: string): boolean {
  const byId = intentRegistry.get(appId);
  if (!byId) return false;
  for (const t of byId.values()) {
    if (t === intentType) return true;
  }
  return false;
}

// ── Harness lifecycle ───────────────────────────────────────────────────

export async function ensureConnected(): Promise<void> {
  if (harnessStarted) {
    const { quickConnectCheck } = await import("./doc-bridge.js");
    if (await quickConnectCheck()) return;
    harnessStarted = false;
    harnessReady = null;
  }
  if (!harnessReady) {
    harnessReady = (async () => {
      startHarness(process.cwd());
      await connectToHarness();
      harnessStarted = true;
    })();
  }
  await harnessReady;
}

export function disposeExtensionState(): void {
  intentRegistry.clear();
}

// ── Intent op plumbing ──────────────────────────────────────────────────

function errResult(text: string) {
  return { content: [{ type: "text" as const, text }], details: {}, isError: true };
}
function okResult(text: string, details: Record<string, unknown> = {}) {
  return { content: [{ type: "text" as const, text }], details, isError: false };
}

type ToolResult = ReturnType<typeof okResult | typeof errResult>;

async function sendIntentOp(op: IntentOp): Promise<ToolResult> {
  try {
    await ensureConnected();
  } catch {
    return errResult("A2Web unavailable.");
  }
  const op_id = `op-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  sendToHarness({
    type: "intent_op",
    op_id,
    kind: op.kind,
    app_id: op.app_id,
    intent_id: op.intent_id,
    data: op.data ?? null,
    target_app: op.target_app ?? null,
  });

  return new Promise((resolve) => {
    const timeout = setTimeout(() => {
      resolve(
        errResult(
          `Timed out waiting for the intent response from app '${op.app_id}'. ` +
            `The app may not handle 'intent-request' events.`,
        ),
      );
    }, 20_000);

    const unsub = onMessage((m: HarnessMessage) => {
      if (m.type !== "intent_response" || m.op_id !== op_id) return;
      clearTimeout(timeout);
      unsub();
      if (m.is_error) {
        resolve(errResult(`Error: ${m.error ?? "unknown error"}`));
      } else if (op.kind === "get" && m.data) {
        resolve(okResult(m.data, { app_id: m.app_id, intent_id: m.intent_id }));
      } else {
        resolve(okResult(`Done.`, { app_id: m.app_id, intent_id: m.intent_id }));
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
      "Launch a web app in a new native window (webview). The app HTML is fully rendered with the A2Web intent polyfill (document.modelContext.registerIntent etc.).",
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
    name: "get_intent",
    label: "Get Intent Content",
    description:
      "Read the content of an intent held by a web app (for 'notes' intents: the title and content). Provide the app_id and the intent id. " +
      "The intent must already be registered — you will have seen an '[A2Web] Intent registered' message in your context. " +
      "Only call this when the user explicitly asks you to read an intent's content. " +
      "Do NOT fetch content as part of a transfer between apps — use send_intent_to for that, which moves content app-to-app without exposing it to you.",
    parameters: Type.Object({
      app: Type.String({ description: "app_id of the web app holding the intent" }),
      id: Type.String({ description: "intent id" }),
    }),
    async execute(_id: string, params: any) {
      const type = knownIntent(params.app, params.id);
      if (type === undefined && intentRegistry.has(params.app)) {
        return errResult(`App '${params.app}' has no registered intent '${params.id}'.`);
      }
      return sendIntentOp({ kind: "get", app_id: params.app, intent_id: params.id });
    },
  });

  pi.registerTool({
    name: "set_intent",
    label: "Set Intent Content",
    description:
      "Write content into an intent held by a web app. Provide the app_id, the intent id, and the new content matching the intent's scheme " +
      "(for 'notes': {\"title\": ..., \"content\": ...} — the id field is filled in automatically). " +
      "The intent must already be registered. Only call this when the user explicitly asks you to write or update an intent's content.",
    parameters: Type.Object({
      app: Type.String({ description: "app_id of the web app holding the intent" }),
      id: Type.String({ description: "intent id" }),
      content: Type.Object(
        {
          title: Type.String({ description: "Note title" }),
          content: Type.String({ description: "Note body text" }),
        },
        { additionalProperties: false },
      ),
    }),
    async execute(_id: string, params: any) {
      const type = knownIntent(params.app, params.id);
      if (type === undefined && intentRegistry.has(params.app)) {
        return errResult(`App '${params.app}' has no registered intent '${params.id}'.`);
      }
      return sendIntentOp({
        kind: "set",
        app_id: params.app,
        intent_id: params.id,
        data: JSON.stringify(params.content),
      });
    },
  });

  pi.registerTool({
    name: "send_intent_to",
    label: "Send Intent To",
    description:
      "Clone an intent's content from one web app (src_app) into another web app (target_app). " +
      "Both apps must have registered an intent of the same type (e.g. both 'notes'). " +
      "The content is transferred directly between the two apps and is NEVER shown to you — " +
      "after a successful transfer a new '[A2Web] Intent registered' message for the target app (same id, same type) appears in your context. " +
      "Only call this when the user asks to move or copy content between apps.",
    parameters: Type.Object({
      src_app: Type.String({ description: "app_id of the app that currently holds the intent" }),
      id: Type.String({ description: "intent id to clone from src_app" }),
      target_app: Type.String({ description: "app_id of the app to receive the cloned intent" }),
    }),
    async execute(_id: string, params: any) {
      const srcType = knownIntent(params.src_app, params.id);
      if (srcType === undefined && intentRegistry.has(params.src_app)) {
        return errResult(`App '${params.src_app}' has no registered intent '${params.id}'.`);
      }
      if (intentRegistry.has(params.target_app) && !appHasIntentType(params.target_app, srcType ?? "")) {
        return errResult(
          `App '${params.target_app}' has no intent of type '${srcType ?? "unknown"}' — ` +
            `both apps must register the same intent type for a transfer.`,
        );
      }
      return sendIntentOp({
        kind: "transfer",
        app_id: params.src_app,
        intent_id: params.id,
        target_app: params.target_app,
      });
    },
  });
}
