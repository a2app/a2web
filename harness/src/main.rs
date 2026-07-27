use std::env;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket as AxumWs, WebSocketUpgrade};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};

use samod::{ConnDirection, DocHandle};
use serde::{Deserialize, Serialize};
use shared::{AgentDoc, WebViewStatus, JSON_WS_PORT, SAMOD_WS_PORT};
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum PiToHarnessMsg {
    #[serde(rename = "launch_webview")]
    LaunchWebView { app_id: String, html: String },
    #[serde(rename = "close_webview")]
    CloseWebView { app_id: String },
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
    #[serde(rename = "state_changed")]
    StateChanged,
    #[serde(rename = "state")]
    State { webviews: Vec<serde_json::Value>, tools: Vec<serde_json::Value>, observations: Vec<serde_json::Value> },
}

fn main() {
    let _ = tracing_subscriber::fmt::try_init();
    let headless = env::var("HARNESS_HEADLESS").ok().as_deref() == Some("1");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
        rt.block_on(background_main(headless));
    });
    loop { std::thread::sleep(Duration::from_secs(3600)); }
}

async fn background_main(headless: bool) {
    let repo = samod::Repo::build_tokio().load().await;
    let mut initial = automerge::Automerge::new();
    {
        let mut tx = initial.transaction();
        autosurgeon::reconcile(&mut tx, &AgentDoc::default()).expect("reconcile");
        tx.commit();
    }
    let doc_handle = repo.create(initial).await.expect("create document");
    let doc_id = doc_handle.document_id().to_string();
    println!("[harness] doc {doc_id}");

    // CRDT WS server
    let repo2 = repo.clone();
    tokio::spawn(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], SAMOD_WS_PORT));
        kill_on_port(SAMOD_WS_PORT);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                loop {
                    match l.accept().await {
                        Ok((s, _)) => {
                            let r = repo2.clone();
                            tokio::spawn(async move {
                                if let Ok(ws) = tokio_tungstenite::accept_async(s).await {
                                    let _ = r.connect_tungstenite(ws, ConnDirection::Incoming);
                                }
                            });
                        }
                        Err(e) => { eprintln!("[harness] accept: {e}"); break; }
                    }
                }
            }
            Err(e) => eprintln!("[harness] bind: {e}"),
        }
    });

    // Spawn web-host
    if !headless {
        let bin = env::current_exe().ok().map(|b| {
            let mut p = b.parent().unwrap().to_path_buf();
            p.push("web-host");
            if !p.exists() {
                let mut p2 = b.parent().unwrap().parent().unwrap().to_path_buf();
                p2.push("web-host"); p2.push("target"); p2.push("debug"); p2.push("web-host");
                return p2;
            }
            p
        }).unwrap_or_else(|| {
            let mut p = env::current_dir().unwrap_or_default();
            p.push("target"); p.push("debug"); p.push("web-host"); p
        });
        if let Ok(child) = Command::new(&bin)
            .env("A2WEB_DOC_ID", &doc_id)
            .env("A2WEB_WS_URL", format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit())
            .spawn()
        {
            println!("[harness] web-host spawned");
            // We don't track the child — it'll connect and sync via CRDT
            let _ = child;
        }
    }

    // JSON WS for pi
    let bridge = Arc::new(Mutex::new(BridgeState {
        doc: doc_handle.clone(),
        pi_tx: tokio::sync::broadcast::channel(64).0,
    }));

    let b2 = bridge.clone();
    tokio::spawn(async move {
        let app = Router::new().route("/", get(move |ws: WebSocketUpgrade| async move {
            ws.on_upgrade(move |s| async move { handle_pi(s, b2).await; })
        }));
        kill_on_port(JSON_WS_PORT);
        if let Ok(l) = tokio::net::TcpListener::bind(SocketAddr::from(([127,0,0,1], JSON_WS_PORT))).await {
            let _ = axum::serve(l, app).await;
        }
    });

    // Bridge loop
    let mut changes = doc_handle.changes();
    while let Some(_) = changes.next().await {
        let should_exit = doc_handle.with_document(|doc| {
            use autosurgeon::hydrate;
            let a: AgentDoc = hydrate(doc).unwrap_or_default();
            a.should_exit
        });
        if should_exit { break; }
        let json = serde_json::to_string(&HarnessToPiMsg::StateChanged).unwrap_or_default();
        let _ = bridge.lock().await.pi_tx.send(json);
    }
    println!("[harness] done");
}

struct BridgeState {
    doc: DocHandle,
    pi_tx: tokio::sync::broadcast::Sender<String>,
}

async fn handle_pi(ws: AxumWs, bridge: Arc<Mutex<BridgeState>>) {
    let (mut tx, mut rx) = ws.split();
    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        while let Some(msg) = fwd_rx.recv().await {
            if tx.send(Message::Text(msg.into())).await.is_err() { break; }
        }
    });

    fwd_tx.send(serde_json::to_string(&HarnessToPiMsg::Welcome).unwrap()).ok();
    let mut pi_rx = bridge.lock().await.pi_tx.subscribe();
    let fwd2 = fwd_tx.clone();
    tokio::spawn(async move { while let Ok(m) = pi_rx.recv().await { fwd2.send(m).ok(); } });

    let doc = bridge.lock().await.doc.clone();
    while let Some(Ok(msg)) = rx.next().await {
        let text = match msg { Message::Text(t) => t.to_string(), _ => continue };
        let parsed: PiToHarnessMsg = match serde_json::from_str(&text) { Ok(m) => m, Err(_) => continue };

        match parsed {
            PiToHarnessMsg::LaunchWebView { app_id, html } => {
                doc.with_document(|d| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                    a.webviews.push(shared::LaunchedWebView { id: app_id, html, status: WebViewStatus::Pending });
                    let mut t = d.transaction();
                    let _ = reconcile(&mut t, &a);
                    t.commit();
                });
            }
            PiToHarnessMsg::GetState => {
                let json = doc.with_document(|d| {
                    use autosurgeon::hydrate;
                    let a: AgentDoc = hydrate(d).unwrap_or_default();
                    let w: Vec<_> = a.webviews.iter().map(|w| serde_json::json!({"id": w.id, "status": format!("{:?}", w.status)})).collect();
                    let t: Vec<_> = a.registered_tools.iter().map(|t| serde_json::json!(t)).collect();
                    let o: Vec<_> = a.observations.iter().map(|o| serde_json::json!(o)).collect();
                    serde_json::to_string(&HarnessToPiMsg::State { webviews: w, tools: t, observations: o }).unwrap_or_default()
                });
                fwd_tx.send(json).ok();
            }
            PiToHarnessMsg::CloseWebView { app_id } => {
                doc.with_document(|d| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                    a.webviews.retain(|w| w.id != app_id);
                    a.registered_tools.retain(|t| t.app_id != app_id);
                    let mut t = d.transaction();
                    let _ = reconcile(&mut t, &a);
                    t.commit();
                });
            }
            PiToHarnessMsg::Exit => {
                doc.with_document(|d| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                    a.should_exit = true;
                    let mut t = d.transaction();
                    let _ = reconcile(&mut t, &a);
                    t.commit();
                });
                break;
            }
        }
    }
}

fn kill_on_port(port: u16) {
    if let Ok(o) = std::process::Command::new("lsof").args(["-ti", &format!(":{port}")]).output() {
        for pid in String::from_utf8_lossy(&o.stdout).lines() {
            let _ = std::process::Command::new("kill").args(["-9", pid.trim()]).status();
        }
    }
}
