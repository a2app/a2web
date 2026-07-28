// ── A2Web WebMCP Polyfill ───────────────────────────────────────────────
//
// Minimal implementation of the WebMCP API (https://webmachinelearning.github.io/webmcp/)
// with one extension: ModelContext.sendObservation()
//
// This polyfill provides:
//   document.modelContext             - WebMCP ModelContext interface
//   document.modelContext.registerTool(tool, options?) -> Promise<void>
//   document.modelContext.getTools(options?) -> Promise<RegisteredTool[]>
//   document.modelContext.sendObservation(data, label?) -> void   (extension to spec)
//   document.modelContext.ontoolchange - EventHandler for toolchange events
//   'toolchange' event                - Fired when tools are added/removed
//
// Tools registered here are automatically synced to the pi agent via CRDT.
// Observations sent via sendObservation() are delivered to the pi agent.
// Tool calls from pi arrive via the CRDT and are dispatched to execute() callbacks.

(function () {
  'use strict';

  // ── Internal state ──────────────────────────────────────────────────
  const toolMap = new Map();        // name -> { definition, execute }
  const changeListeners = new Set(); // callbacks for tool change events

  // ── CRDT sync interface (set by web-host) ────────────────────────────
  // The Rust host sets these before loading the app HTML.
  window.__a2web = window.__a2web || {};

  // __a2web.syncTools(tools: Array<{name, title, description, inputSchema}>)
  //   Called when tool registrations change — pushes to CRDT.
  // __a2web.onToolCall(callId, toolName, args) -> Promise<any>
  //   Called when pi invokes a tool — dispatches to the registered handler.
  // __a2web.sendObservation(appId, data, label)
  //   Called to push an observation into the CRDT.
  // __a2web.getToolsSnapshot() -> Array
  //   Called periodically by host to sync registered tools.

  // ── Tool Definition struct ───────────────────────────────────────────
  function createToolDefinition(tool) {
    const stringifiedSchema = tool.inputSchema
      ? JSON.stringify(tool.inputSchema)
      : '';

    return {
      name: tool.name,
      title: tool.title || null,
      description: tool.description,
      inputSchema: stringifiedSchema,
      execute: tool.execute,
      annotations: tool.annotations || { readOnlyHint: false, untrustedContentHint: false },
    };
  }

  // ── RegisteredTool struct ────────────────────────────────────────────
  function createRegisteredTool(toolDef, win, origin) {
    return {
      name: toolDef.name,
      title: toolDef.title || '',
      description: toolDef.description,
      inputSchema: toolDef.inputSchema,
      window: win,
      origin: origin,
      annotations: toolDef.annotations,
    };
  }

  // ── ModelContext Implementation ──────────────────────────────────────
  class ModelContextImpl extends EventTarget {
    constructor() {
      super();
      this._internalContext = { toolMap: new Map() };
      this._origin = window.origin || 'null';
      this._appId = window.__a2web.appId || 'unknown';
    }

    // registerTool(tool, options?)
    async registerTool(tool, options = {}) {
      // Validation
      if (!tool.name || typeof tool.name !== 'string') {
        return Promise.reject(new DOMException('Tool name is required', 'InvalidStateError'));
      }
      if (tool.name.length < 1 || tool.name.length > 128) {
        return Promise.reject(new DOMException('Tool name must be 1-128 characters', 'InvalidStateError'));
      }
      if (!/^[a-zA-Z0-9_.-]+$/.test(tool.name)) {
        return Promise.reject(new DOMException('Tool name contains invalid characters', 'InvalidStateError'));
      }
      if (!tool.description || typeof tool.description !== 'string') {
        return Promise.reject(new DOMException('Tool description is required', 'InvalidStateError'));
      }
      if (typeof tool.execute !== 'function') {
        return Promise.reject(new DOMException('Tool execute callback is required', 'InvalidStateError'));
      }

      // Check for duplicates
      if (this._internalContext.toolMap.has(tool.name)) {
        return Promise.reject(new DOMException('Tool with this name is already registered', 'InvalidStateError'));
      }

      // Validate inputSchema if provided
      let stringifiedSchema = '';
      if (tool.inputSchema !== undefined) {
        try {
          stringifiedSchema = JSON.stringify(tool.inputSchema);
        } catch (e) {
          return Promise.reject(new TypeError('inputSchema could not be serialized: ' + e.message));
        }
      }

      // Create tool definition
      const toolDef = {
        name: tool.name,
        title: tool.title || null,
        description: tool.description,
        inputSchema: stringifiedSchema,
        executeSteps: tool.execute,
        annotations: {
          readOnlyHint: tool.annotations?.readOnlyHint || false,
          untrustedContentHint: tool.annotations?.untrustedContentHint || false,
        },
        exposedOrigins: [],
      };

      // Handle options.exposedTo
      if (options.exposedTo) {
        for (const origin of options.exposedTo) {
          try {
            const url = new URL(origin);
            if (url.protocol !== 'https:' && url.protocol !== 'http:') {
              return Promise.reject(new DOMException('Invalid or untrusted origin', 'SecurityError'));
            }
            toolDef.exposedOrigins.push(url.origin);
          } catch {
            return Promise.reject(new DOMException('Invalid origin URL', 'SecurityError'));
          }
        }
      }

      // Handle AbortSignal
      if (options.signal) {
        if (options.signal.aborted) {
          return Promise.reject(options.signal.reason || new DOMException('Aborted', 'AbortError'));
        }
        options.signal.addEventListener('abort', () => {
          this._unregisterTool(tool.name);
        });
      }

      // Store
      this._internalContext.toolMap.set(tool.name, toolDef);

      // Notify toolchange
      this._dispatchToolChange();

      // Sync to CRDT
      this._syncToolsToCRDT();

      return Promise.resolve(undefined);
    }

    // getTools(options?)
    async getTools(options = {}) {
      const fromOrigins = options.fromOrigins || [];
      const tools = [];

      // Collect from all documents in the frame tree
      const collectTools = (doc) => {
        if (!doc || !doc.modelContext) return;
        const ctx = doc.modelContext._internalContext;
        const docOrigin = doc.defaultView?.origin || 'null';
        const docWin = doc.defaultView;

        for (const [name, toolDef] of ctx.toolMap) {
          // Check if exposed to caller
          const callerOrigin = window.origin;
          if (docOrigin === callerOrigin || fromOrigins.includes(docOrigin)) {
            // Check exposed origins
            if (toolDef.exposedOrigins.length > 0 && !toolDef.exposedOrigins.includes(callerOrigin)) {
              if (docOrigin !== callerOrigin) continue;
            }

            tools.push({
              name: toolDef.name,
              title: toolDef.title || '',
              description: toolDef.description,
              inputSchema: toolDef.inputSchema,
              window: docWin,
              origin: docOrigin,
              annotations: { ...toolDef.annotations },
            });
          }
        }
      };

      collectTools(document);

      // Collect from iframes
      const iframes = document.querySelectorAll('iframe');
      for (const iframe of iframes) {
        try {
          if (iframe.contentDocument) {
            collectTools(iframe.contentDocument);
          }
        } catch {
          // Cross-origin iframe — skip
        }
      }

      // Sort by name
      tools.sort((a, b) => a.name.localeCompare(b.name));

      return Promise.resolve(tools);
    }

    // ── sendObservation — extension to WebMCP spec ────────────────────
    // Sends an observation from this web app directly to the pi agent.
    // The observation is written into the CRDT and forwarded to pi.
    //
    sendObservation(data, label) {
      if (typeof data === 'object') {
        data = JSON.stringify(data);
      }
      const obs = {
        appId: this._appId,
        data: String(data),
        label: label || null,
        sequence: Date.now(),
      };
      if (window.__a2web && typeof window.__a2web.sendObservation === 'function') {
        window.__a2web.sendObservation(obs);
      }
    }

    // ── Internal ──────────────────────────────────────────────────────

    _unregisterTool(name) {
      this._internalContext.toolMap.delete(name);
      this._dispatchToolChange();
      this._syncToolsToCRDT();
    }

    _dispatchToolChange() {
      const event = new Event('toolchange');
      this.dispatchEvent(event);
    }

    _syncToolsToCRDT() {
      if (window.__a2web && typeof window.__a2web.syncTools === 'function') {
        const tools = [];
        for (const [name, def] of this._internalContext.toolMap) {
          tools.push({
            name: def.name,
            title: def.title,
            description: def.description,
            inputSchema: def.inputSchema,
          });
        }
        window.__a2web.syncTools(tools);
      }
    }

    // Called by web-host when pi invokes a tool
    async _executeTool(callId, toolName, args) {
      const toolDef = this._internalContext.toolMap.get(toolName);
      if (!toolDef) {
        return { error: `Tool '${toolName}' not found` };
      }

      try {
        const result = await toolDef.executeSteps(args);
        return { result };
      } catch (e) {
        return { error: e.message || String(e) };
      }
    }
  }

  // ── Install on document ──────────────────────────────────────────────
  if (!document.modelContext) {
    const mc = new ModelContextImpl();
    Object.defineProperty(document, 'modelContext', {
      value: mc,
      writable: false,
      configurable: false,
    });

    // Wire up the host bridge
    if (window.__a2web) {
      const origOnToolCall = window.__a2web.onToolCall;
      window.__a2web.onToolCall = async function (callId, toolName, args) {
        const result = await mc._executeTool(callId, toolName, args);
        if (origOnToolCall) {
          origOnToolCall(callId, toolName, args, result);
        }
        return result;
      };
    }
  }
})();
