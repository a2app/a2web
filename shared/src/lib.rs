use autosurgeon::{Hydrate, Reconcile};
use serde::{Deserialize, Serialize};

/// JSON WebSocket port — pi extension ↔ harness (plain JSON, no CRDT)
pub const JSON_WS_PORT: u16 = 2341;

/// samod WebSocket port — harness ↔ web-host (CRDT sync between Rust processes)
pub const SAMOD_WS_PORT: u16 = 2342;

// ── Intent schemes ─────────────────────────────────────────────────────
//
// An intent is a typed, schematized unit of data that a web app registers
// as available. Registration announces only `{type, id}` — the content never
// enters the agent context. Content is read/written through dedicated tools
// and transferred directly between apps.

/// The only supported intent type for now.
pub const INTENT_TYPE_NOTES: &str = "notes";

/// Validates that an intent type is a known, supported scheme.
pub fn is_valid_intent_type(t: &str) -> bool {
    t == INTENT_TYPE_NOTES
}

/// Validates an intent id (must be a non-empty string, max 256 chars).
pub fn is_valid_intent_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}

/// Validates intent content against its scheme and fills in the `id` field
/// so it always matches the registered intent id.
///
/// The `notes` scheme: `{ id: string, title: string, content: string }`.
pub fn normalize_intent_data(intent_id: &str, data: &serde_json::Value) -> Option<serde_json::Value> {
    let mut obj = data.as_object()?.clone();
    let title = obj.get("title").and_then(|x| x.as_str())?;
    if title.is_empty() {
        return None;
    }
    if obj.get("content").and_then(|x| x.as_str()).is_none() {
        return None;
    }
    obj.insert("id".to_string(), serde_json::Value::String(intent_id.to_string()));
    Some(serde_json::Value::Object(obj))
}

// ── AgentDoc: The shared CRDT document ──────────────────────────────────

#[derive(Debug, Clone, Default, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct AgentDoc {
    /// Webviews that the pi extension wants launched.
    pub webviews: Vec<LaunchedWebView>,

    /// Intents registered by web apps — the system-side registry used for
    /// validation (does an app actually hold this intent? does the target
    /// app support the same scheme?).
    pub intents: Vec<RegisteredIntent>,

    /// Pending intent operations queued by pi (get / set / transfer).
    /// Written by the harness, consumed (and cleared) by the web-host.
    pub intent_ops: Vec<PendingIntentOp>,

    /// Resolved intent operations written by the web-host and forwarded to pi.
    /// Never cleared (same pattern as observations before them).
    pub intent_responses: Vec<IntentResponse>,

    /// Intent registrations to forward to pi — both app-initiated
    /// `registerIntent` calls and transfer completions (a cloned intent now
    /// exists in the target app). Never cleared.
    pub intent_registrations: Vec<IntentRegistration>,

    /// Intent unregistrations to forward to pi (app called `unregisterIntent`).
    /// Never cleared.
    pub intent_unregistrations: Vec<IntentRegistration>,

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

/// An intent a web app has made available: `{type, id}` only — no content.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct RegisteredIntent {
    pub app_id: String,
    pub intent_type: String,
    pub intent_id: String,
}

/// An intent operation queued by pi for the web-host to execute against apps.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct PendingIntentOp {
    /// Unique id correlating this op with its response.
    pub op_id: String,
    /// "get" | "set" | "transfer"
    pub kind: String,
    /// Source app — for get/set it is the app holding the intent, for
    /// transfer it is the app the content is cloned from.
    pub app_id: String,
    pub intent_id: String,
    /// JSON content for "set" ops (provided by the agent).
    pub data: Option<String>,
    /// Destination app for "transfer" ops.
    pub target_app: Option<String>,
}

/// Result of an intent op, written by the web-host.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct IntentResponse {
    /// Matches the PendingIntentOp.op_id
    pub op_id: String,
    pub kind: String,
    pub app_id: String,
    pub intent_id: String,
    /// JSON content (only for successful "get" ops)
    pub data: Option<String>,
    pub is_error: bool,
    pub error: Option<String>,
}

/// An intent registration to announce to the agent: either the app called
/// `registerIntent(type, id)` or a transfer cloned an intent into the app.
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct IntentRegistration {
    pub app_id: String,
    pub intent_type: String,
    pub intent_id: String,
}
