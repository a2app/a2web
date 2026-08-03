use autosurgeon::{Hydrate, Reconcile};
use serde::{Deserialize, Serialize};

/// JSON WebSocket port — pi extension ↔ harness (plain JSON, no CRDT)
pub const JSON_WS_PORT: u16 = 2341;

/// samod WebSocket port — harness ↔ web-host (CRDT sync between Rust processes)
pub const SAMOD_WS_PORT: u16 = 2342;

// ── Intent & entity model ───────────────────────────────────────────────
//
// An **intent** is a capability an app advertises: "this app handles notes".
// An **entity** is a specific instance linked to an intent: each note is a
// separate entity. The agent is told about capabilities and entity ids only
// (never content). Content is read/written/moved through dedicated tools.

/// The only supported intent type for now.
pub const INTENT_TYPE_NOTES: &str = "notes";

/// Validates that an intent type is a known, supported scheme.
pub fn is_valid_intent_type(t: &str) -> bool {
    t == INTENT_TYPE_NOTES
}

/// Validates an entity id (must be a non-empty string, max 256 chars).
pub fn is_valid_entity_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}

/// Validates entity content against its scheme and fills in the `id` field
/// so it always matches the entity id.
///
/// The `notes` scheme: `{ id: string, title: string, content: string }`.
pub fn normalize_entity_data(entity_id: &str, data: &serde_json::Value) -> Option<serde_json::Value> {
    let mut obj = data.as_object()?.clone();
    let title = obj.get("title").and_then(|x| x.as_str())?;
    if title.is_empty() {
        return None;
    }
    if obj.get("content").and_then(|x| x.as_str()).is_none() {
        return None;
    }
    obj.insert("id".to_string(), serde_json::Value::String(entity_id.to_string()));
    Some(serde_json::Value::Object(obj))
}

// ── AgentDoc: The shared CRDT document ──────────────────────────────────

#[derive(Debug, Clone, Default, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct AgentDoc {
    /// Webviews that the pi extension wants launched.
    pub webviews: Vec<LaunchedWebView>,

    /// Intent capabilities registered by apps: `{app_id, intent_type}`.
    pub intents: Vec<RegisteredIntent>,

    /// Entities linked to intents: `{app_id, intent_type, entity_id}` —
    /// each specific note an app holds.
    pub entities: Vec<RegisteredEntity>,

    /// Pending entity ops queued by pi (list / read / set / transfer).
    /// Written by the harness, consumed (and cleared) by the web-host.
    pub entity_ops: Vec<PendingEntityOp>,

    /// Resolved entity ops written by the web-host and forwarded to pi.
    /// Never cleared (same pattern as observations before them).
    pub entity_responses: Vec<EntityResponse>,

    /// Announcements for the agent context — intent/entity registered or
    /// unregistered (type + id only, never content). Never cleared.
    pub announcements: Vec<Announcement>,

    /// Set to true by pi to request graceful shutdown.
    pub should_exit: bool,

    /// Error message from web-host.
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct LaunchedWebView {
    pub id: String,
    pub html: String,
    pub status: WebViewStatus,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub enum WebViewStatus {
    Pending,
    Launched,
    Closed,
}

/// An intent capability an app has registered: "this app handles `notes`".
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct RegisteredIntent {
    pub app_id: String,
    pub intent_type: String,
}

/// A specific entity linked to an intent: one note inside a notes app.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct RegisteredEntity {
    pub app_id: String,
    pub intent_type: String,
    pub entity_id: String,
}

/// An entity op queued by pi for the web-host to execute against apps.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct PendingEntityOp {
    /// Unique id correlating this op with its response.
    pub op_id: String,
    /// "list" | "read" | "set" | "transfer"
    pub kind: String,
    /// Source app — for list/read/set it is the app holding the entities,
    /// for transfer it is the app the entity is moved from.
    pub app_id: String,
    pub intent_type: String,
    /// Empty string for "list" ops.
    pub entity_id: String,
    /// JSON content for "set" ops (provided by the agent).
    pub data: Option<String>,
    /// Destination app for "transfer" ops.
    pub target_app: Option<String>,
}

/// Result of an entity op, written by the web-host.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct EntityResponse {
    /// Matches the PendingEntityOp.op_id
    pub op_id: String,
    pub kind: String,
    pub app_id: String,
    pub intent_type: String,
    /// Empty for "list" ops.
    pub entity_id: String,
    /// JSON content — a single entity for "read", an array for "list".
    pub data: Option<String>,
    pub is_error: bool,
    pub error: Option<String>,
}

/// What kind of registration change to announce to the agent.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub enum AnnouncementKind {
    IntentRegistered,
    IntentUnregistered,
    EntityRegistered,
    EntityUnregistered,
}

impl AnnouncementKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnnouncementKind::IntentRegistered => "intent_registered",
            AnnouncementKind::IntentUnregistered => "intent_unregistered",
            AnnouncementKind::EntityRegistered => "entity_registered",
            AnnouncementKind::EntityUnregistered => "entity_unregistered",
        }
    }
}

/// A registration change to announce to the agent context (type + id only).
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    pub kind: AnnouncementKind,
    pub app_id: String,
    pub intent_type: String,
    /// Set for entity announcements.
    pub entity_id: Option<String>,
}
