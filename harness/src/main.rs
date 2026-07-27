use std::env;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket as AxumWs, WebSocketUpgrade};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use samod::{ConnDirection, DocHandle};
use serde::{Deserialize, Serialize};
use shared::{
    AgentDoc, Observation, PendingToolCall, RegisteredTool, ToolResult, WebViewStatus,
    JSON_WS_PORT, SAMOD_WS_PORT,
};
use tokio::sync::Mutex;

// ── JSON WS message types (pi ↔ harness) ─────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum PiToHarnessMsg {
    #[serde(rename = "launch_webview")]
    LaunchWebView { app_id: String, html: String },
    #[serde(rename = "close_webview")]
    CloseWebView { app_id: String },
    #[serde(rename = "invoke_tool")]
    InvokeTool {
        call_id: String,
        tool_name: String,
        arguments: String,
        app_id: String,
    },
    #[serde(rename = "tool_result_consumed")]
    ToolResultConsumed { call_id: String, app_id: String },
    #[serde(rename = "get_state")]
    GetState,
    #[serde(rename = "exit")]
    Exit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum HarnessToPiMsg {
    #[serde(rename = "welcome")]
    Welcome,
    #[serde(rename = "webview_launched")]
    WebViewLaunched { app_id: String, status: String },
    #[serde(rename = "tool_registered")]
    ToolRegistered { tools: Vec<RegisteredTool> },
    #[serde(rename = "observation")]
    ObservationData { observations: Vec<Observation> },
    #[serde(rename = "tool_result")]
    ToolResultData {
        call_id: String,
        app_id: String,
        result: String,
        is_error: bool,
    },
    #[serde(rename = "state")]
    State {
        webviews: Vec<serde_json::Value>,
        tools: Vec<RegisteredTool>,
        observations: Vec<Observation>,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

// ── Main ────────────────────────────────────────────────────────────────

fn main() {
    let _ = tracing_subscriber::fmt::try_init();
    let headless = env::var("HARNESS_HEADLESS").ok().as_deref() == Some("1");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
        rt.block_on(background_main(headless));
    });

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

async fn background_main(headless: bool) {
    // ── 1. Create samod repo and shared doc ──────────────────────────
    let repo = samod::Repo::build_tokio().load().await;
    let mut initial = automerge::Automerge::new();
    {
        let mut tx = initial.transaction();
        autosurgeon::reconcile(&mut tx, &AgentDoc::default()).expect("reconcile");
        tx.commit();
    }
    let doc_handle = repo.create(initial).await.expect("create shared document");
    let doc_id = doc_handle.document_id().to_string();
    println!("[harness] shared doc ID: {doc_id}");

    // Clear all fields
    doc_handle.with_document(|doc| {
        use autosurgeon::{hydrate, reconcile};
        let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
        agent.webviews.clear();
        agent.registered_tools.clear();
        agent.observations.clear();
        agent.tool_results.clear();
        agent.tool_calls.clear();
        agent.should_exit = false;
        let mut tx = doc.transaction();
        reconcile(&mut tx, &agent).expect("reconcile");
        tx.commit();
    });

    // ── 2. Set up samod WS server for web-host ───────────────────────
    kill_process_on_port(SAMOD_WS_PORT);
    let repo_samod = repo.clone();
    tokio::spawn(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], SAMOD_WS_PORT));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                println!("[harness] samod WS listening on 127.0.0.1:{SAMOD_WS_PORT}");
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let repo = repo_samod.clone();
                            tokio::spawn(async move {
                                let ws_stream = match tokio_tungstenite::accept_async(stream).await
                                {
                                    Ok(ws) => ws,
                                    Err(e) => {
                                        eprintln!("[harness] WS accept error: {e}");
                                        return;
                                    }
                                };
                                if let Err(e) =
                                    repo.connect_tungstenite(ws_stream, ConnDirection::Incoming)
                                {
                                    eprintln!("[harness] samod connect error: {e:?}");
                                }
                            });
                        }
                        Err(e) => {
                            eprintln!("[harness] samod WS accept: {e}");
                            break;
                        }
                    }
                }
            }
            Err(e) => eprintln!("[harness] samod WS bind: {e}"),
        }
    });

    // ── 3. Spawn web-host ────────────────────────────────────────────
    let mut web_host_child: Option<Child> = None;
    if !headless {
        let harness_bin = env::current_exe().ok();
        let host_bin = if let Some(ref bin) = harness_bin {
            let mut p = bin.parent().unwrap().to_path_buf();
            p.push("web-host");
            if p.exists() {
                p
            } else {
                let mut p = bin.parent().unwrap().parent().unwrap().to_path_buf();
                p.push("web-host");
                p.push("target");
                p.push("debug");
                p.push("web-host");
                p
            }
        } else {
            let mut p = env::current_dir().unwrap_or_default();
            p.push("target");
            p.push("debug");
            p.push("web-host");
            p
        };

        println!("[harness] spawning web-host: {}", host_bin.display());
        match Command::new(&host_bin)
            .env("A2WEB_DOC_ID", &doc_id)
            .env(
                "A2WEB_WS_URL",
                format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                web_host_child = Some(child);
                println!("[harness] web-host spawned");
            }
            Err(e) => {
                eprintln!("[harness] failed to spawn web-host: {e}");
            }
        }
    }

    // ── 4. Set up JSON WS server for pi extension ────────────────────
    let bridge_state = BridgeState {
        doc: doc_handle.clone(),
        pi_tx: tokio::sync::broadcast::channel(64).0,
    };
    let bridge = Arc::new(Mutex::new(bridge_state));

    let bridge_for_ws = bridge.clone();
    tokio::spawn(async move {
        let ws_app = Router::new().route(
            "/",
            get(move |ws: WebSocketUpgrade| {
                let bridge = bridge_for_ws.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        handle_pi_ws(socket, bridge).await;
                    })
                }
            }),
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], JSON_WS_PORT));
        kill_process_on_port(JSON_WS_PORT);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                println!("[harness] JSON WS listening on 127.0.0.1:{JSON_WS_PORT}");
                let _ = axum::serve(listener, ws_app).await;
            }
            Err(e) => eprintln!("[harness] JSON WS bind: {e}"),
        }
    });

    // ── 5. Bridge loop: doc changes → push to pi ────────────────────
    let mut doc_changes = doc_handle.changes();
    let mut last_observation_count: usize = 0;
    let mut last_tool_count: usize = 0;
    let mut last_tool_result_count: usize = 0;
    let mut last_launched_apps: Vec<(String, String)> = Vec::new();

    while let Some(_change) = doc_changes.next().await {
        eprintln!("[harness] doc change detected");
        // Check web-host alive
        if let Some(ref mut child) = web_host_child {
            if let Ok(Some(_status)) = child.try_wait() {
                eprintln!("[harness] web-host process exited");
                break;
            }
        }

        // Read doc state
        let (launched_apps, obs_count, tools_count, results_count, exit) = doc_handle
            .with_document(|doc| {
                use autosurgeon::hydrate;
                let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                let apps: Vec<(String, String)> = agent
                    .webviews
                    .iter()
                    .map(|wv| (wv.id.clone(), format!("{:?}", wv.status)))
                    .collect();
                (
                    apps,
                    agent.observations.len(),
                    agent.registered_tools.len(),
                    agent.tool_results.len(),
                    agent.should_exit,
                )
            });

        // Push webview status changes
        if launched_apps != last_launched_apps {
            for (id, st) in &launched_apps {
                let changed = match last_launched_apps.iter().find(|(pid, _)| pid == id) {
                    Some((_, prev)) => prev != st,
                    None => true,
                };
                if changed {
                    let msg = HarnessToPiMsg::WebViewLaunched {
                        app_id: id.clone(),
                        status: st.clone(),
                    };
                    let json = serde_json::to_string(&msg).unwrap_or_default();
                    let _ = bridge.lock().await.pi_tx.send(json);
                }
            }
            last_launched_apps = launched_apps;
        }

        // Push new observations
        if obs_count > last_observation_count {
            let new_obs: Vec<Observation> = doc_handle.with_document(|doc| {
                use autosurgeon::hydrate;
                let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                agent
                    .observations
                    .iter()
                    .skip(last_observation_count)
                    .cloned()
                    .collect()
            });
            if !new_obs.is_empty() {
                let msg = HarnessToPiMsg::ObservationData {
                    observations: new_obs,
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                let _ = bridge.lock().await.pi_tx.send(json);
            }
            last_observation_count = obs_count;
        }

        // Push new tool registrations
        if tools_count > last_tool_count {
            let tools: Vec<RegisteredTool> = doc_handle.with_document(|doc| {
                use autosurgeon::hydrate;
                let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                agent.registered_tools.clone()
            });
            let msg = HarnessToPiMsg::ToolRegistered { tools };
            let json = serde_json::to_string(&msg).unwrap_or_default();
            let _ = bridge.lock().await.pi_tx.send(json);
            last_tool_count = tools_count;
        }

        // Push tool results
        if results_count > last_tool_result_count {
            let new_results: Vec<ToolResult> = doc_handle.with_document(|doc| {
                use autosurgeon::hydrate;
                let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                agent
                    .tool_results
                    .iter()
                    .skip(last_tool_result_count)
                    .cloned()
                    .collect()
            });
            for result in &new_results {
                let msg = HarnessToPiMsg::ToolResultData {
                    call_id: result.call_id.clone(),
                    app_id: result.app_id.clone(),
                    result: result.result.clone(),
                    is_error: result.is_error,
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                let _ = bridge.lock().await.pi_tx.send(json);
            }
            last_tool_result_count = results_count;
        }

        if exit {
            println!("[harness] should_exit — stopping");
            break;
        }
    }

    if let Some(mut child) = web_host_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    println!("[harness] bridge loop ended");
}

// ── Bridge state ─────────────────────────────────────────────────────────

struct BridgeState {
    doc: DocHandle,
    pi_tx: tokio::sync::broadcast::Sender<String>,
}

// ── Handle pi WebSocket connection ───────────────────────────────────────

async fn handle_pi_ws(ws: AxumWs, bridge: Arc<Mutex<BridgeState>>) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let fwd_handle = tokio::spawn(async move {
        while let Some(msg) = fwd_rx.recv().await {
            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let send_to_pi = |msg: HarnessToPiMsg| {
        let json = serde_json::to_string(&msg).unwrap_or_default();
        let _ = fwd_tx.send(json);
    };

    send_to_pi(HarnessToPiMsg::Welcome);
    println!("[harness] pi connected");

    let mut pi_rx = bridge.lock().await.pi_tx.subscribe();
    let fwd_tx2 = fwd_tx.clone();
    tokio::spawn(async move {
        while let Ok(msg) = pi_rx.recv().await {
            let _ = fwd_tx2.send(msg);
        }
    });

    let doc_handle = bridge.lock().await.doc.clone();
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };

        let parsed: PiToHarnessMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[harness] bad JSON from pi: {e}");
                continue;
            }
        };

        match parsed {
            PiToHarnessMsg::LaunchWebView { app_id, html } => {
                println!("[harness] pi: launch webview '{app_id}'");
                doc_handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    agent.webviews.push(shared::LaunchedWebView {
                        id: app_id,
                        html,
                        status: WebViewStatus::Pending,
                    });
                    let mut tx = doc.transaction();
                    let _ = reconcile(&mut tx, &agent);
                    tx.commit();
                });
            }

            PiToHarnessMsg::CloseWebView { app_id } => {
                println!("[harness] pi: close webview '{app_id}'");
                doc_handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    agent.webviews.retain(|wv| wv.id != app_id);
                    agent.registered_tools.retain(|t| t.app_id != app_id);
                    let mut tx = doc.transaction();
                    let _ = reconcile(&mut tx, &agent);
                    tx.commit();
                });
            }

            PiToHarnessMsg::InvokeTool {
                call_id,
                tool_name,
                arguments,
                app_id,
            } => {
                println!("[harness] pi: invoke tool '{tool_name}' on app '{app_id}'");
                doc_handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    agent.tool_calls.push(PendingToolCall {
                        id: call_id,
                        tool_name,
                        arguments,
                        app_id,
                    });
                    let mut tx = doc.transaction();
                    let _ = reconcile(&mut tx, &agent);
                    tx.commit();
                });
            }

            PiToHarnessMsg::ToolResultConsumed { call_id, app_id } => {
                doc_handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    agent
                        .tool_results
                        .retain(|r| r.call_id != call_id || r.app_id != app_id);
                    let mut tx = doc.transaction();
                    let _ = reconcile(&mut tx, &agent);
                    tx.commit();
                });
            }

            PiToHarnessMsg::GetState => {
                let json = doc_handle.with_document(|doc| {
                    use autosurgeon::hydrate;
                    let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    let webviews: Vec<serde_json::Value> = agent.webviews.iter()
                        .map(|wv| serde_json::json!({"id": wv.id, "status": format!("{:?}", wv.status)}))
                        .collect();
                    let msg = HarnessToPiMsg::State {
                        webviews, tools: agent.registered_tools.clone(), observations: agent.observations.clone(),
                    };
                    serde_json::to_string(&msg).unwrap_or_default()
                });
                let _ = fwd_tx.send(json);
            }

            PiToHarnessMsg::Exit => {
                println!("[harness] pi: exit");
                doc_handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                    agent.should_exit = true;
                    let mut tx = doc.transaction();
                    let _ = reconcile(&mut tx, &agent);
                    tx.commit();
                });
                break;
            }
        }
    }

    println!("[harness] pi disconnected");
    fwd_handle.abort();
}

fn kill_process_on_port(port: u16) {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output();
    if let Ok(output) = output {
        if !output.stdout.is_empty() {
            let pids = String::from_utf8_lossy(&output.stdout);
            for pid in pids.lines() {
                let pid = pid.trim();
                if !pid.is_empty() {
                    let _ = std::process::Command::new("kill")
                        .args(["-9", pid])
                        .status();
                }
            }
        }
    }
}
