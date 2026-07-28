import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { stopHarness } from "./harness.js";
import { registerTools, startBackgroundListener, disposeAllSessions } from "./tools.js";
import { sendToHarness } from "./doc-bridge.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ADDENDUM = readFileSync(join(__dirname, "agent-addendum.md"), "utf-8");

export default async function (pi: ExtensionAPI): Promise<void> {
  registerTools(pi);
  startBackgroundListener();

  pi.on("session_start", async (_event: any, ctx: any) => ctx.ui.setStatus("a2web", "A2Web: idle"));
  pi.on("session_shutdown", async () => { try { sendToHarness({ type: "exit" }); } catch {} disposeAllSessions(); stopHarness(); });
  pi.on("before_agent_start", async (event: any) => ({
    systemPrompt: `${event.systemPrompt}\n\n${ADDENDUM}`,
  }));
}
