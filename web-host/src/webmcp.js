// ── A2Web WebMCP Polyfill — Intent API ─────────────────────────────────
//
// Second iteration: tools and observations are gone. Web apps now register
// *intents* — typed, schematized data they can receive, hold and share.
// Registering an intent announces only its `type` and `id` to the agent;
// the actual content never enters the agent context unless the agent
// explicitly calls get_intent (and then it sees the content in the tool
// result only).
//
// This polyfill provides (on document.modelContext):
//   registerIntent(type, id) -> Promise<void>
//       Announces `{type, id}` to the system and agent. Validates the type
//       against the known intent schemes (only "notes" for now) and the id.
//   sendIntent(type, id, data, requestId) -> void
//       Sends intent content to the system. Used to respond to an
//       `intent-request` event (the system asks the app for an intent's
//       content so it can be read by the agent or cloned to another app).
//   getIntent(type, id) -> data | undefined
//       Local lookup of an intent's content (used inside intent-request
//       handlers). Does not contact the system.
//   setIntent(type, id, data) -> data
//       Stores content locally in the app. Does NOT notify the agent —
//       content is app-private until a request or transfer happens.
//   getIntents() -> [{type, id}, ...]
//       Lists intents this app registered.
//
// Events the system fires into the app:
//   'intent-request'  CustomEvent { requestId, type, id }
//       The system wants the content of intent `id`. The app must respond
//       with document.modelContext.sendIntent(type, id, data, requestId).
//   'intent-receive'  CustomEvent { type, id, data, source }
//       The system is delivering content into this app — either from a
//       send_intent_to transfer (source: "transfer") or from the agent
//       calling set_intent (source: "agent"). The app stores/render it
//       with document.modelContext.setIntent(type, id, data).
//
// The Rust host sets window.__a2web before loading app HTML.

(function () {
  'use strict';

  // ── Intent schemes ──────────────────────────────────────────────────
  // Only "notes" for now. A scheme lists the fields an intent's content
  // must have. The `id` field is always filled in automatically to match
  // the registered intent id.
  const INTENT_SCHEMAS = {
    notes: {
      type: 'notes',
      description: 'A note with a title and body text.',
      fields: {
        id: { type: 'string', description: 'Intent id (filled automatically)' },
        title: { type: 'string', description: 'Note title' },
        content: { type: 'string', description: 'Note body text' },
      },
    },
  };

  // ── Internal state ──────────────────────────────────────────────────
  const intentMap = new Map();   // "type::id" -> { type, id }
  const contentMap = new Map();  // "type::id" -> data (app-private)

  const keyOf = (type, id) => `${type}::${id}`;

  function isValidIntentType(type) {
    return Object.prototype.hasOwnProperty.call(INTENT_SCHEMAS, type);
  }

  function isValidIntentId(id) {
    return typeof id === 'string' && id.length > 0 && id.length <= 256;
  }

  // Validate + normalize intent content against its scheme. The `id` field
  // is forced to match the registered intent id.
  function normalizeData(type, id, data) {
    if (!isValidIntentType(type)) {
      return { ok: false, error: `Unknown intent type '${type}'` };
    }
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { ok: false, error: 'Intent content must be an object' };
    }
    const out = Object.assign({}, data, { id });
    const fields = INTENT_SCHEMAS[type].fields;
    for (const fname of Object.keys(fields)) {
      if (fname === 'id') continue;
      const f = fields[fname];
      if (f.type === 'string') {
        if (typeof out[fname] !== 'string') {
          return { ok: false, error: `Intent content field '${fname}' must be a string` };
        }
        if (fname === 'title' && out[fname].length === 0) {
          return { ok: false, error: `Intent content field 'title' must not be empty` };
        }
      }
    }
    return { ok: true, data: out };
  }

  // ── ModelContext Implementation ─────────────────────────────────────
  class ModelContextImpl extends EventTarget {
    constructor() {
      super();
      this._appId = (window.__a2web && window.__a2web.appId) || 'unknown';
    }

    // registerIntent(type, id)
    //   Announces `{type, id}` to the system (and therefore to the agent's
    //   context). Only the type and id are sent — never content.
    async registerIntent(type, id) {
      if (!isValidIntentType(type)) {
        return Promise.reject(
          new DOMException(
            `Unknown intent type '${type}'. Supported types: ${Object.keys(INTENT_SCHEMAS).join(', ')}`,
            'InvalidStateError',
          ),
        );
      }
      if (!isValidIntentId(id)) {
        return Promise.reject(
          new DOMException('Intent id must be a non-empty string (max 256 chars)', 'InvalidStateError'),
        );
      }
      const k = keyOf(type, id);
      if (intentMap.has(k)) {
        return Promise.reject(
          new DOMException(`Intent '${id}' of type '${type}' is already registered`, 'InvalidStateError'),
        );
      }
      intentMap.set(k, { type, id });
      if (window.__a2web && typeof window.__a2web.registerIntent === 'function') {
        window.__a2web.registerIntent({ type, id });
      }
      return Promise.resolve(undefined);
    }

    // unregisterIntent(type, id)
    //   Removes the intent from this app and announces the removal to the
    //   system (and therefore the agent's context). Content is discarded.
    async unregisterIntent(type, id) {
      const k = keyOf(type, id);
      if (!intentMap.has(k)) {
        return Promise.reject(
          new DOMException(`Intent '${id}' of type '${type}' is not registered`, 'InvalidStateError'),
        );
      }
      intentMap.delete(k);
      contentMap.delete(k);
      if (window.__a2web && typeof window.__a2web.unregisterIntent === 'function') {
        window.__a2web.unregisterIntent({ type, id });
      }
      return Promise.resolve(undefined);
    }

    // sendIntent(type, id, data, requestId)
    //   Sends the content of intent `id` to the system, answering an
    //   'intent-request' event. The system decides where the content goes
    //   (back to the agent for get_intent, or on to another app for
    //   send_intent_to).
    //
    //   The app does not need to have *registered* this exact intent id — a
    //   cloned intent delivered via 'intent-receive' is held but not
    //   registered. The app just needs to have the content.
    sendIntent(type, id, data, requestId) {
      const n = normalizeData(type, id, data);
      if (!n.ok) throw new TypeError(n.error);
      contentMap.set(keyOf(type, id), n.data);
      if (window.__a2web && typeof window.__a2web.sendIntentContent === 'function') {
        window.__a2web.sendIntentContent(requestId, { type, id, data: n.data });
      }
    }

    // getIntent(type, id)
    //   Local-only lookup of an intent's content. No system round-trip.
    getIntent(type, id) {
      return contentMap.get(keyOf(type, id));
    }

    // setIntent(type, id, data)
    //   Store intent content locally in this app. App-private — never
    //   announced to the agent. The app's 'intent-receive' handler uses
    //   this to adopt content delivered by a transfer or set_intent.
    setIntent(type, id, data) {
      const n = normalizeData(type, id, data);
      if (!n.ok) throw new TypeError(n.error);
      contentMap.set(keyOf(type, id), n.data);
      return n.data;
    }

    // getIntents()
    //   Lists the intents this app has registered.
    getIntents() {
      return Array.from(intentMap.values());
    }
  }

  // ── Install on document ─────────────────────────────────────────────
  if (!document.modelContext) {
    Object.defineProperty(document, 'modelContext', {
      value: new ModelContextImpl(),
      writable: false,
      configurable: false,
    });
  }
})();
