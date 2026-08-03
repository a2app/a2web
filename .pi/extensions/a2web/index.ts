import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { stopHarness } from "./harness.js";
import {
  registerTools,
  registerIntentInRegistry,
  unregisterIntentInRegistry,
  registerEntityInRegistry,
  unregisterEntityInRegistry,
  disposeExtensionState,
  statsText,
  setStatusCallback,
} from "./tools.js";
import { sendToHarness, onMessage } from "./doc-bridge.js";
import type { AnnouncementMessage, HarnessMessage } from "./types.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ADDENDUM = readFileSync(join(__dirname, "agent-addendum.md"), "utf-8");

// ── Footer status ───────────────────────────────────────────────────────
// `ui` comes from the session_start event context; we keep a reference so
// the background listener can update the footer as registrations arrive.

let uiRef: any = null;

function setStatus(text: string): void {
  try {
    uiRef?.setStatus("a2web", text);
  } catch {
    // ui may not be available outside a session
  }
}

function refreshStatus(): void {
  setStatus(statsText());
}

// ── Background listener: forward registrations to the MAIN agent ────────
//
// Every registration change (an app announces it handles an intent type, or
// an entity — a specific note — is registered/unregistered, or a transfer
// clones an entity into the target app) is injected into the main agent's
// context. Only type + ids, never content. This is what lets the main agent
// act proactively — directly, no sub-agent.

function announcementText(a: AnnouncementMessage): string {
  switch (a.kind) {
    case "intent_registered":
      return `[A2Web] Intent registered — app: ${a.app_id}, type: ${a.intent_type}`;
    case "intent_unregistered":
      return `[A2Web] Intent unregistered — app: ${a.app_id}, type: ${a.intent_type}`;
    case "entity_registered":
      return `[A2Web] Entity registered — app: ${a.app_id}, intent: ${a.intent_type}, id: ${a.entity_id}`;
    case "entity_unregistered":
      return `[A2Web] Entity unregistered — app: ${a.app_id}, intent: ${a.intent_type}, id: ${a.entity_id}`;
  }
}

function handleAnnouncement(pi: ExtensionAPI, a: AnnouncementMessage): void {
  switch (a.kind) {
    case "intent_registered":
      registerIntentInRegistry(a.app_id, a.intent_type);
      break;
    case "intent_unregistered":
      unregisterIntentInRegistry(a.app_id, a.intent_type);
      break;
    case "entity_registered":
      if (a.entity_id) registerEntityInRegistry(a.app_id, a.intent_type, a.entity_id);
      break;
    case "entity_unregistered":
      if (a.entity_id) unregisterEntityInRegistry(a.app_id, a.entity_id);
      break;
  }

  try {
    pi.sendMessage(
      {
        customType: "a2web-intent",
        content: announcementText(a),
        display: true,
        details: {
          app_id: a.app_id,
          intent_type: a.intent_type,
          entity_id: a.entity_id,
        },
      },
      { deliverAs: "followUp", triggerTurn: true },
    );
  } catch (e) {
    console.error("[A2Web] failed to inject announcement into context:", e);
  }
  refreshStatus();
}

function startBackgroundListener(pi: ExtensionAPI): void {
  onMessage((msg: HarnessMessage) => {
    if (msg.type === "announcement") {
      handleAnnouncement(pi, msg);
    }
  });
}

export default async function (pi: ExtensionAPI): Promise<void> {
  setStatusCallback((text) => setStatus(text));
  registerTools(pi);
  startBackgroundListener(pi);

  pi.on("session_start", async (_event: any, ctx: any) => {
    uiRef = ctx.ui;
    setStatus("A2Web: ready");
  });
  pi.on("session_shutdown", async () => {
    try {
      sendToHarness({ type: "exit" });
    } catch {}
    disposeExtensionState();
    stopHarness();
  });
  pi.on("before_agent_start", async (event: any) => ({
    systemPrompt: `${event.systemPrompt}\n\n${ADDENDUM}`,
  }));
}
