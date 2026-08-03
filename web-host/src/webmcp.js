// ── A2Web WebMCP Polyfill — Intent & Entity API ─────────────────────────
//
// Third iteration: an **intent** is a capability an app advertises ("this app
// handles notes"); an **entity** is a specific instance linked to that intent
// (each note is a separate entity). Registering announces only the type and
// ids to the agent — the actual content never enters the agent context unless
// the agent explicitly reads it.
//
// This polyfill provides (on document.modelContext):
//   registerIntent(type) -> Promise<void>
//       Announces the capability "I handle <type>" (e.g. 'notes').
//   unregisterIntent(type) -> Promise<void>
//       Removes the capability and every entity linked to it.
//   registerEntity(type, id) -> Promise<void>
//       Announces an entity: one specific note of type `type`. Requires the
//       intent to be registered first.
//   unregisterEntity(type, id) -> Promise<void>
//       Removes an entity (e.g. after a transfer moved it away).
//   sendEntity(type, id, data, requestId) -> void
//       Sends one entity's content to the system, answering an
//       'entity-request' event (modes "read" / "transfer").
//   sendEntities(type, entities, requestId) -> void
//       Sends all entities of a type to the system, answering an
//       'entity-request' event with mode "list".
//   getEntity(type, id) / setEntity(type, id, data) -> data
//       App-local content store (never announced to the agent).
//   getEntities(type) -> [id, ...]
//       App-local list of entity ids for a type.
//
// Events the system fires into the app:
//   'entity-request'  CustomEvent { requestId, type, id, mode }
//       mode "list": app must respond via sendEntities(type, [{id, data}],
//         requestId). mode "read"/"transfer": app responds via
//         sendEntity(type, id, data, requestId). On "transfer" the app may
//         delete the entity locally (it is now owned by another app) —
//         deletion is the app's choice.
//   'entity-receive'  CustomEvent { type, id, data, source }
//       System delivers entity content into this app — from a send_entity_to
//       transfer (source: "transfer") or from the agent's set_entity
//       (source: "agent").
//
// The Rust host sets window.__a2web before loading app HTML.

(function () {
  'use strict';

  // ── Intent schemes ──────────────────────────────────────────────────
  // Only "notes" for now. A scheme lists the fields an entity's content
  // must have. The `id` field is always filled in automatically to match
  // the entity id.
  const INTENT_SCHEMAS = {
    notes: {
      type: 'notes',
      description: 'A note with a title and body text.',
      fields: {
        id: { type: 'string', description: 'Entity id (filled automatically)' },
        title: { type: 'string', description: 'Note title' },
        content: { type: 'string', description: 'Note body text' },
      },
    },
  };

  // ── Internal state ──────────────────────────────────────────────────
  const intentSet = new Set();   // type strings — capabilities
  const entityMap = new Map();   // "type::id" -> { type, id }
  const contentMap = new Map();  // "type::id" -> data (app-private)

  const keyOf = (type, id) => `${type}::${id}`;

  function isValidIntentType(type) {
    return Object.prototype.hasOwnProperty.call(INTENT_SCHEMAS, type);
  }

  function isValidEntityId(id) {
    return typeof id === 'string' && id.length > 0 && id.length <= 256;
  }

  // Validate + normalize entity content against its scheme. The `id` field
  // is forced to match the entity id.
  function normalizeData(type, id, data) {
    if (!isValidIntentType(type)) {
      return { ok: false, error: `Unknown intent type '${type}'` };
    }
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { ok: false, error: 'Entity content must be an object' };
    }
    const out = Object.assign({}, data, { id });
    const fields = INTENT_SCHEMAS[type].fields;
    for (const fname of Object.keys(fields)) {
      if (fname === 'id') continue;
      const f = fields[fname];
      if (f.type === 'string') {
        if (typeof out[fname] !== 'string') {
          return { ok: false, error: `Entity content field '${fname}' must be a string` };
        }
        if (fname === 'title' && out[fname].length === 0) {
          return { ok: false, error: `Entity content field 'title' must not be empty` };
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

    // registerIntent(type)
    //   Announces the capability "this app handles <type>". Only the type
    //   is sent to the agent.
    async registerIntent(type) {
      if (!isValidIntentType(type)) {
        return Promise.reject(
          new DOMException(
            `Unknown intent type '${type}'. Supported types: ${Object.keys(INTENT_SCHEMAS).join(', ')}`,
            'InvalidStateError',
          ),
        );
      }
      if (intentSet.has(type)) {
        return Promise.reject(
          new DOMException(`Intent '${type}' is already registered`, 'InvalidStateError'),
        );
      }
      intentSet.add(type);
      if (window.__a2web && typeof window.__a2web.registerIntent === 'function') {
        window.__a2web.registerIntent({ type });
      }
      return Promise.resolve(undefined);
    }

    // unregisterIntent(type)
    //   Removes the capability and every entity linked to it.
    async unregisterIntent(type) {
      if (!intentSet.has(type)) {
        return Promise.reject(
          new DOMException(`Intent '${type}' is not registered`, 'InvalidStateError'),
        );
      }
      intentSet.delete(type);
      // Drop all entities of this type first (each announces its removal).
      for (const [k, ent] of Array.from(entityMap.entries())) {
        if (ent.type === type) {
          entityMap.delete(k);
          contentMap.delete(k);
          if (window.__a2web && typeof window.__a2web.unregisterEntity === 'function') {
            window.__a2web.unregisterEntity({ type, id: ent.id });
          }
        }
      }
      if (window.__a2web && typeof window.__a2web.unregisterIntent === 'function') {
        window.__a2web.unregisterIntent({ type });
      }
      return Promise.resolve(undefined);
    }

    // registerEntity(type, id)
    //   Announces one specific entity of `type`. Requires the intent
    //   capability to be registered first. Idempotent — registering an
    //   entity that already exists resolves without error.
    async registerEntity(type, id) {
      if (!intentSet.has(type)) {
        return Promise.reject(
          new DOMException(
            `Intent '${type}' is not registered — call registerIntent('${type}') first`,
            'InvalidStateError',
          ),
        );
      }
      if (!isValidEntityId(id)) {
        return Promise.reject(
          new DOMException('Entity id must be a non-empty string (max 256 chars)', 'InvalidStateError'),
        );
      }
      const k = keyOf(type, id);
      if (!entityMap.has(k)) {
        entityMap.set(k, { type, id });
        if (window.__a2web && typeof window.__a2web.registerEntity === 'function') {
          window.__a2web.registerEntity({ type, id });
        }
      }
      return Promise.resolve(undefined);
    }

    // unregisterEntity(type, id)
    //   Removes an entity, announcing the removal to the agent.
    async unregisterEntity(type, id) {
      const k = keyOf(type, id);
      if (!entityMap.has(k)) {
        return Promise.reject(
          new DOMException(`Entity '${id}' of type '${type}' is not registered`, 'InvalidStateError'),
        );
      }
      entityMap.delete(k);
      contentMap.delete(k);
      if (window.__a2web && typeof window.__a2web.unregisterEntity === 'function') {
        window.__a2web.unregisterEntity({ type, id });
      }
      return Promise.resolve(undefined);
    }

    // sendEntity(type, id, data, requestId)
    //   Sends one entity's content to the system, answering an
    //   'entity-request' (mode "read" or "transfer").
    sendEntity(type, id, data, requestId) {
      const n = normalizeData(type, id, data);
      if (!n.ok) throw new TypeError(n.error);
      contentMap.set(keyOf(type, id), n.data);
      if (window.__a2web && typeof window.__a2web.sendEntityContent === 'function') {
        window.__a2web.sendEntityContent(requestId, { type, entities: [{ id, data: n.data }] });
      }
    }

    // sendEntities(type, entities, requestId)
    //   Sends all entities of a type, answering an 'entity-request' with
    //   mode "list". `entities` is an array of { id, data }.
    sendEntities(type, entities, requestId) {
      if (!Array.isArray(entities)) {
        throw new TypeError('entities must be an array of { id, data }');
      }
      const out = [];
      for (const e of entities) {
        const n = normalizeData(type, e.id, e.data);
        if (!n.ok) throw new TypeError(n.error);
        contentMap.set(keyOf(type, e.id), n.data);
        out.push({ id: e.id, data: n.data });
      }
      if (window.__a2web && typeof window.__a2web.sendEntityContent === 'function') {
        window.__a2web.sendEntityContent(requestId, { type, entities: out });
      }
    }

    // setEntity(type, id, data)
    //   Store entity content locally in this app. Content is app-private —
    //   never announced. If the entity is new it is also registered (the
    //   entity's existence is announced; the web-host dedupes repeat
    //   announcements, e.g. when a transfer already announced the clone).
    setEntity(type, id, data) {
      if (!isValidIntentType(type)) {
        throw new TypeError(`Unknown intent type '${type}'`);
      }
      if (!intentSet.has(type)) {
        throw new DOMException(
          `Intent '${type}' is not registered — call registerIntent('${type}') first`,
          'InvalidStateError',
        );
      }
      const n = normalizeData(type, id, data);
      if (!n.ok) throw new TypeError(n.error);
      const k = keyOf(type, id);
      const isNew = !entityMap.has(k);
      contentMap.set(k, n.data);
      if (isNew) {
        entityMap.set(k, { type, id });
        if (window.__a2web && typeof window.__a2web.registerEntity === 'function') {
          window.__a2web.registerEntity({ type, id });
        }
      }
      return n.data;
    }

    // getEntity(type, id)
    //   App-local lookup of an entity's content. No system round-trip.
    getEntity(type, id) {
      return contentMap.get(keyOf(type, id));
    }

    // getEntities(type)
    //   App-local list of entity ids registered for a type.
    getEntities(type) {
      return Array.from(entityMap.values())
        .filter((e) => e.type === type)
        .map((e) => e.id);
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
