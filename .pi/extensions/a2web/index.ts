import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { stopHarness } from "./harness.js";
import { registerTools, registerIntentInRegistry, unregisterIntentInRegistry, disposeExtensionState } from "./tools.js";
import { sendToHarness, onMessage } from "./doc-bridge.js";
import type { HarnessMessage } from "./types.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ADDENDUM = readFileSync(join(__dirname, "agent-addendum.md"), "utf-8");

// ── Background listener: forward intent registrations to the MAIN agent ─
//
// Every time an intent is registered (either by an app calling
// registerIntent, or by a transfer cloning an intent into the target app),
// we inject the `{type, id}` announcement into the main agent's context.
// The content is never included. This is what lets the main agent act
// proactively — exactly what the old sub-agent system did, but directly.

function startBackgroundListener(pi: ExtensionAPI): void {
  onMessage((msg: HarnessMessage) => {
    if (msg.type === "intent_registered") {
      registerIntentInRegistry(msg.app_id, msg.intent_type, msg.intent_id);
      injectContext(
        pi,
        `[A2Web] Intent registered — app: ${msg.app_id}, type: ${msg.intent_type}, id: ${msg.intent_id}`,
        msg,
      );
    } else if (msg.type === "intent_unregistered") {
      unregisterIntentInRegistry(msg.app_id, msg.intent_id);
      injectContext(
        pi,
        `[A2Web] Intent unregistered — app: ${msg.app_id}, type: ${msg.intent_type}, id: ${msg.intent_id}`,
        msg,
      );
    }
  });
}

function injectContext(
  pi: ExtensionAPI,
  content: string,
  details: { app_id: string; intent_type: string; intent_id: string },
): void {
  try {
    pi.sendMessage(
      {
        customType: "a2web-intent",
        content,
        display: true,
        details: {
          app_id: details.app_id,
          intent_type: details.intent_type,
          intent_id: details.intent_id,
        },
      },
      { deliverAs: "followUp", triggerTurn: true },
    );
  } catch (e) {
    console.error("[A2Web] failed to inject intent message into context:", e);
  }
}

export default async function (pi: ExtensionAPI): Promise<void> {
  registerTools(pi);
  startBackgroundListener(pi);

  pi.on("session_start", async (_event: any, ctx: any) => ctx.ui.setStatus("a2web", "A2Web: idle"));
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
