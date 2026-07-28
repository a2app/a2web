import WebSocket from "ws";
import type { HarnessMessage } from "./types.js";

const HARNESS_WS = "ws://127.0.0.1:2341/";
const CONNECT_TIMEOUT_MS = 15_000;
const CONNECT_RETRY_MS = 500;
const QUICK_TIMEOUT_MS = 1_500;

let ws: WebSocket | null = null;
let messageHandlers: Array<(msg: HarnessMessage) => void> = [];
let connectedResolve: (() => void) | null = null;
let connectedPromise: Promise<void> | null = null;

// ── Message handlers ────────────────────────────────────────────────────

export function onMessage(handler: (msg: HarnessMessage) => void): () => void {
  messageHandlers.push(handler);
  return () => {
    messageHandlers = messageHandlers.filter((h) => h !== handler);
  };
}

// ── Connection management ────────────────────────────────────────────────

export async function connectToHarness(): Promise<void> {
  if (connectedPromise) return connectedPromise;

  connectedPromise = new Promise<void>((resolve, reject) => {
    const deadline = Date.now() + CONNECT_TIMEOUT_MS;

    const tryConnect = () => {
      if (Date.now() > deadline) {
        connectedPromise = null;
        reject(new Error("Timed out connecting to harness"));
        return;
      }

      const socket = new WebSocket(HARNESS_WS);
      let settled = false;

      const cleanRetry = () => {
        if (settled) return;
        settled = true;
        socket.close();
        setTimeout(tryConnect, CONNECT_RETRY_MS);
      };

      socket.on("open", () => {
        ws = socket;
        connectedResolve = resolve;
      });

      socket.on("message", (data: Buffer) => {
        try {
          const msg = JSON.parse(data.toString()) as HarnessMessage;
          if (msg.type === "welcome") {
            if (connectedResolve) {
              connectedResolve();
              connectedResolve = null;
            }
          }

          for (const handler of messageHandlers) {
            handler(msg);
          }
        } catch (err) {
          // ignore parse errors
        }
      });

      socket.on("close", () => {
        ws = null;
        cleanRetry();
      });

      socket.on("error", () => {
        socket.close();
        cleanRetry();
      });

      const connectTimeout = setTimeout(() => {
        if (ws?.readyState !== WebSocket.OPEN) {
          cleanRetry();
        }
      }, CONNECT_RETRY_MS);

      socket.on("open", () => {
        clearTimeout(connectTimeout);
      });
    };

    tryConnect();
  });

  return connectedPromise;
}

export async function quickConnectCheck(): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const socket = new WebSocket(HARNESS_WS);
    const timer = setTimeout(() => {
      socket.close();
      resolve(false);
    }, QUICK_TIMEOUT_MS);
    socket.on("open", () => {
      clearTimeout(timer);
      socket.close();
      resolve(true);
    });
    socket.on("error", () => {
      clearTimeout(timer);
      resolve(false);
    });
  });
}

export function sendToHarness(msg: object): void {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(msg));
  }
}
