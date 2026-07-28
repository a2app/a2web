import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { stopHarness } from "./harness.js";
import { registerTools, startBackgroundListener, disposeAllSessions } from "./tools.js";
import { sendToHarness } from "./doc-bridge.js";

const ADDENDUM = `
## A2Web: Web App Agent System

You have access to web applications that run in native webviews. Each webview has a WebMCP polyfill that allows the app to register tools and send observations.

### Your Tools

**start_sub_agent** — Create a sub-agent session. Returns a session_id. Multiple webviews can share the same sub-agent.

**launch_webview** — Launch a web app in a new native window (webview). Optionally link to a sub-agent session.

**invoke_webapp_tool** — Call a tool that a web app has registered.

### Proactive Behavior

Observations from web apps arrive directly as context. Act on them proactively.
`;

export default async function (pi: ExtensionAPI): Promise<void> {
  registerTools(pi);
  startBackgroundListener();

  pi.on("session_start", async (_event: any, ctx: any) => ctx.ui.setStatus("a2web", "A2Web: idle"));
  pi.on("session_shutdown", async () => { try { sendToHarness({ type: "exit" }); } catch {} disposeAllSessions(); stopHarness(); });
  pi.on("before_agent_start", async (event: any) => ({
    systemPrompt: `${event.systemPrompt}\n\n${ADDENDUM}`,
  }));
}
