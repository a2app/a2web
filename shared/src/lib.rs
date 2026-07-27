use autosurgeon::{Hydrate, Reconcile};
use serde::{Deserialize, Serialize};

/// JSON WebSocket port — pi extension ↔ harness (plain JSON, no CRDT)
pub const JSON_WS_PORT: u16 = 2341;

/// samod WebSocket port — harness ↔ web-host (CRDT sync between Rust processes)
pub const SAMOD_WS_PORT: u16 = 2342;

// ── AgentDoc: The shared CRDT document ──────────────────────────────────

#[derive(Debug, Clone, Default, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct AgentDoc {
    /// Webviews that the pi extension wants launched.
    pub webviews: Vec<LaunchedWebView>,

    /// Tools registered by web apps, keyed by tool name.
    /// Each webview can register tools; they get forwarded to pi.
    pub registered_tools: Vec<RegisteredTool>,

    /// Observations sent by web apps via sendObservation.
    /// Each observation is tagged with the webview/app_id that sent it.
    pub observations: Vec<Observation>,

    /// Tool call results from pi back to webviews.
    pub tool_results: Vec<ToolResult>,

    /// Pending tool calls to be picked up by webviews.
    pub tool_calls: Vec<PendingToolCall>,

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

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct RegisteredTool {
    /// Unique name of the tool (e.g. "search_flights")
    pub name: String,
    /// Human-readable title
    pub title: Option<String>,
    /// Natural language description
    pub description: String,
    /// JSON Schema string for input parameters
    pub input_schema: String,
    /// The webview/app_id that registered this tool
    pub app_id: String,
    /// Origin of the document that registered the tool
    pub origin: String,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// The app/webview that sent this observation
    pub app_id: String,
    /// The observation data (JSON-encoded string)
    pub data: String,
    /// Optional label/type for the observation
    pub label: Option<String>,
    /// Monotonically increasing sequence number
    pub sequence: u64,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct PendingToolCall {
    /// Unique ID for this tool call
    pub id: String,
    /// Name of the tool to call
    pub tool_name: String,
    /// JSON-encoded arguments
    pub arguments: String,
    /// The app_id that should handle this call
    pub app_id: String,
}

#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Matches the PendingToolCall.id
    pub call_id: String,
    /// The app_id this result is for
    pub app_id: String,
    /// Tool name
    pub tool_name: String,
    /// JSON-encoded result
    pub result: String,
    /// Whether the tool call errored
    pub is_error: bool,
}
