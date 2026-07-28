import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { Type } from "typebox";
import { connectToHarness, sendToHarness, onMessage } from "./doc-bridge.js";
import { startHarness, stopHarness } from "./harness.js";
import type { HarnessMessage, ObservationInfo } from "./types.js";

type ExtensionAPI = any;
type AgentSession = any;

let harnessStarted = false;
let harnessReady: Promise<void> | null = null;

// ── Sub-agent sessions ──────────────────────────────────────────────────
interface SubSession { session: AgentSession; createdAt: number; }
const subSessions = new Map<string, SubSession>();
// app_id → session_id mapping
const appSessionMap = new Map<string, string>();

const __dirname = dirname(fileURLToPath(import.meta.url));
const SUB_AGENT_PROMPT = readFileSync(join(__dirname, "sub-agent-prompt.md"), "utf-8");

async function forwardObservationToSubAgent(sessionId: string, appId: string, data: string, label: string | null): Promise<void> {
  const stored = subSessions.get(sessionId);
  if (!stored) return;
  try {
    const msg = `[Observation from ${appId}]${label ? ` [${label}]` : ""}: ${data}`;
    stored.session.prompt(msg, { expandPromptTemplates: false, streamingBehavior: 'followUp' }).catch((e: any) => console.error('[A2Web] sub-agent prompt error:', e));
  } catch (e) { console.error('[A2Web] forward error:', e); }
}

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

export function registerTools(pi: ExtensionAPI): void {
  pi.registerTool({
    name: "start_sub_agent",
    label: "Start Sub-Agent",
    description: "Create a sub-agent session. Returns a session_id. Webviews linked to this session send observations directly to it.",
    parameters: Type.Object({
      session_id: Type.Optional(Type.String({ description: "Optional custom session ID" })),
    }),
    async execute(_id: string, params: any) {
      try {
        const { createAgentSession, DefaultResourceLoader, SessionManager, SettingsManager } =
          await import("@earendil-works/pi-coding-agent");
        const { defineTool } = await import("@earendil-works/pi-coding-agent");
        const { tmpdir } = await import("node:os");
        const { mkdirSync, existsSync } = await import("node:fs");
        const { join } = await import("node:path");

        // Tool for sub-agent to invoke webapp tools
        const invokeTool = defineTool({
          name: "invoke_webapp_tool", label: "Invoke WebApp Tool",
          description: "Call a tool that a web app has registered. Provide app_id, tool_name, and JSON arguments. Waits for the result.",
          parameters: Type.Object({ app_id: Type.String(), tool_name: Type.String(), arguments: Type.String() }),
          execute: async (_id2: string, p: any) => {
            try { await ensureConnected(); } catch { return { content: [{ type: "text", text: `Unavailable.` }], details: {}, isError: true }; }
            const cid = `call-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
            sendToHarness({ type: "invoke_tool", call_id: cid, tool_name: p.tool_name, arguments: p.arguments, app_id: p.app_id });
            // Wait for the result observation
            return new Promise((resolve) => {
              const to = setTimeout(() => resolve({ content: [{ type: "text", text: `Timeout waiting for ${p.tool_name} result.` }], details: {}, isError: true }), 20000);
              const u = onMessage((m: any) => {
                if (m.type === "observation" && m.app_id === p.app_id) {
                  try {
                    const d = JSON.parse(m.data);
                    if (d.callId === cid || d.type === "counter") {
                      clearTimeout(to); u();
                      resolve({ content: [{ type: "text", text: m.data }], details: { app_id: p.app_id } });
                    }
                  } catch {}
                }
              });
            });
          },
        });

        const blankDir = join(tmpdir(), "pi-a2web-" + process.pid);
        if (!existsSync(blankDir)) mkdirSync(blankDir, { recursive: true });
        const sm = SettingsManager.create(blankDir, blankDir);
        const loader = new DefaultResourceLoader({
          cwd: blankDir, agentDir: blankDir, settingsManager: sm,
          noContextFiles: true, noSkills: true, noPromptTemplates: true, noThemes: true, noExtensions: true,
          systemPromptOverride: () => SUB_AGENT_PROMPT,
        });
        await loader.reload();
        const { session } = await createAgentSession({
          resourceLoader: loader, sessionManager: SessionManager.inMemory(),
          tools: ["invoke_webapp_tool"], customTools: [invokeTool],
        });

        const sessionId = params.session_id || `sub-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
        subSessions.set(sessionId, { session, createdAt: Date.now() });
        return { content: [{ type: "text", text: JSON.stringify({ session_id: sessionId }) }], details: { session_id: sessionId } };
      } catch (err) {
        return { content: [{ type: "text", text: `Failed: ${err}` }], details: {}, isError: true };
      }
    },
  });

  pi.registerTool({
    name: "launch_webview",
    label: "Launch WebView",
    description: "Launch a web app in a new native window (webview). The app HTML is fully rendered with a WebMCP polyfill.",
    parameters: Type.Object({
      app_id: Type.String({ description: "Unique ID" }),
      html: Type.String({ description: "Full HTML document" }),
      session_id: Type.Optional(Type.String({ description: "Sub-agent session to link" })),
    }),
    async execute(_id: string, params: any) {
      try { await ensureConnected(); } catch { return { content: [{ type: "text", text: `Unavailable.` }], details: {}, isError: true }; }
      if (params.session_id) appSessionMap.set(params.app_id, params.session_id);
      sendToHarness({ type: "launch_webview", app_id: params.app_id, html: params.html });
      return { content: [{ type: "text", text: `Launched '${params.app_id}'.` }], details: { app_id: params.app_id } };
    },
  });

  pi.registerTool({
    name: "invoke_webapp_tool",
    label: "Invoke WebApp Tool",
    description: "Call a tool that a web app has registered.",
    parameters: Type.Object({
      app_id: Type.String(), tool_name: Type.String(), arguments: Type.String(),
    }),
    async execute(_id: string, params: any) {
      try { await ensureConnected(); } catch { return { content: [{ type: "text", text: `Unavailable.` }], details: {}, isError: true }; }
      sendToHarness({ type: "invoke_tool", call_id: `call-${Date.now()}`, tool_name: params.tool_name, arguments: params.arguments, app_id: params.app_id });
      return { content: [{ type: "text", text: `Sent '${params.tool_name}' to '${params.app_id}'.` }], details: {} };
    },
  });
}

export function disposeAllSessions(): void {
  for (const [, s] of subSessions) {
    try { s.session.dispose(); } catch {}
  }
  subSessions.clear();
  appSessionMap.clear();
}

// ── Background: forward observations to linked sub-agents ───────────────

export function startBackgroundListener(): void {
  onMessage((msg: HarnessMessage) => {
    if (msg.type === "observation") {
      const sid = appSessionMap.get(msg.app_id);
      if (sid) {
        forwardObservationToSubAgent(sid, msg.app_id, msg.data, msg.label);
      }
    }
  });
}
