use std::env;
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket as AxumWs, WebSocketUpgrade};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use samod::{ConnDirection, DocHandle};
use serde::{Deserialize, Serialize};
use shared::{AgentDoc, PendingEntityOp, WebViewStatus, JSON_WS_PORT, SAMOD_WS_PORT};
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum PiToHarnessMsg {
    #[serde(rename = "launch_webview")]
    LaunchWebView { app_id: String, html: String },
    #[serde(rename = "close_webview")]
    CloseWebView { app_id: String },
    #[serde(rename = "entity_op")]
    EntityOp {
        op_id: String,
        kind: String,
        app_id: String,
        intent_type: String,
        entity_id: String,
        data: Option<String>,
        target_app: Option<String>,
    },
    #[serde(rename = "exit")]
    Exit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum HarnessToPiMsg {
    #[serde(rename = "welcome")]
    Welcome,
    /// A registration change for the agent context — intent/entity registered
    /// or unregistered. Only type + id, never content.
    #[serde(rename = "announcement")]
    Announcement {
        kind: String,
        app_id: String,
        intent_type: String,
        entity_id: Option<String>,
    },
    /// A queued entity op (list/read/set/transfer) was resolved by the web-host.
    #[serde(rename = "entity_response")]
    EntityResponse {
        op_id: String,
        kind: String,
        app_id: String,
        intent_type: String,
        entity_id: String,
        data: Option<String>,
        is_error: bool,
        error: Option<String>,
    },
}

fn main() {
    let headless = env::var("HARNESS_HEADLESS").ok().as_deref() == Some("1");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(background(headless));
    });
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

async fn background(headless: bool) {
    let repo = samod::Repo::build_tokio().load().await;
    let mut init = automerge::Automerge::new();
    {
        let mut t = init.transaction();
        autosurgeon::reconcile(&mut t, &AgentDoc::default()).unwrap();
        t.commit();
    }
    let dh = repo.create(init).await.unwrap();
    let doc_id = dh.document_id().to_string();
    println!("[harness] doc {doc_id}");

    // CRDT WS server
    let r2 = repo.clone();
    tokio::spawn(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], SAMOD_WS_PORT));
        kill_on(SAMOD_WS_PORT);
        if let Ok(l) = tokio::net::TcpListener::bind(addr).await {
            loop {
                match l.accept().await {
                    Ok((s, _)) => {
                        let r = r2.clone();
                        tokio::spawn(async move {
                            if let Ok(ws) = tokio_tungstenite::accept_async(s).await {
                                let _ = r.connect_tungstenite(ws, ConnDirection::Incoming);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("[harness] {e}");
                        break;
                    }
                }
            }
        }
    });

    // Spawn web-host
    if !headless {
        let bin = env::current_exe()
            .ok()
            .map(|b| {
                let mut p = b.parent().unwrap().to_path_buf();
                p.push("web-host");
                if !p.exists() {
                    let mut p2 = b.parent().unwrap().parent().unwrap().to_path_buf();
                    p2.push("web-host");
                    p2.push("target");
                    p2.push("debug");
                    p2.push("web-host");
                    return p2;
                }
                p
            })
            .unwrap_or_else(|| {
                let mut p = env::current_dir().unwrap_or_default();
                p.push("target");
                p.push("debug");
                p.push("web-host");
                p
            });
        let _ = Command::new(&bin)
            .env("A2WEB_DOC_ID", &doc_id)
            .env(
                "A2WEB_WS_URL",
                format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn();
    }

    // JSON WS for pi
    let bridge = Arc::new(Mutex::new(Bridge {
        dh: dh.clone(),
        pi_tx: tokio::sync::broadcast::channel(64).0,
    }));
    let b2 = bridge.clone();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/",
            get(move |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(move |s| async move {
                    handle_pi(s, b2).await;
                })
            }),
        );
        kill_on(JSON_WS_PORT);
        if let Ok(l) =
            tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], JSON_WS_PORT))).await
        {
            let _ = axum::serve(l, app).await;
        }
    });

    // Bridge: forward new announcements and op responses to pi.
    let mut changes = dh.changes();
    let mut last_ann = 0usize;
    let mut last_resp = 0usize;
    while let Some(_) = changes.next().await {
        let exit = dh.with_document(|d| {
            use autosurgeon::hydrate;
            let a: AgentDoc = hydrate(d).unwrap_or_default();
            a.should_exit
        });
        if exit {
            break;
        }

        let ann_count = dh.with_document(|d| {
            use autosurgeon::hydrate;
            let a: AgentDoc = hydrate(d).unwrap_or_default();
            a.announcements.len()
        });
        if ann_count > last_ann {
            let new_anns = dh.with_document(|d| {
                use autosurgeon::hydrate;
                let a: AgentDoc = hydrate(d).unwrap_or_default();
                a.announcements
                    .iter()
                    .skip(last_ann)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            for r in &new_anns {
                let msg = HarnessToPiMsg::Announcement {
                    kind: r.kind.as_str().to_string(),
                    app_id: r.app_id.clone(),
                    intent_type: r.intent_type.clone(),
                    entity_id: r.entity_id.clone(),
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                let _ = bridge.lock().await.pi_tx.send(json);
            }
            last_ann = ann_count;
        }

        let resp_count = dh.with_document(|d| {
            use autosurgeon::hydrate;
            let a: AgentDoc = hydrate(d).unwrap_or_default();
            a.entity_responses.len()
        });
        if resp_count > last_resp {
            let new_resps = dh.with_document(|d| {
                use autosurgeon::hydrate;
                let a: AgentDoc = hydrate(d).unwrap_or_default();
                a.entity_responses
                    .iter()
                    .skip(last_resp)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            for r in &new_resps {
                let msg = HarnessToPiMsg::EntityResponse {
                    op_id: r.op_id.clone(),
                    kind: r.kind.clone(),
                    app_id: r.app_id.clone(),
                    intent_type: r.intent_type.clone(),
                    entity_id: r.entity_id.clone(),
                    data: r.data.clone(),
                    is_error: r.is_error,
                    error: r.error.clone(),
                };
                let json = serde_json::to_string(&msg).unwrap_or_default();
                let _ = bridge.lock().await.pi_tx.send(json);
            }
            last_resp = resp_count;
        }
    }
}

struct Bridge {
    dh: DocHandle,
    pi_tx: tokio::sync::broadcast::Sender<String>,
}

async fn handle_pi(ws: AxumWs, bridge: Arc<Mutex<Bridge>>) {
    let (mut tx, mut rx) = ws.split();
    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(m) = fwd_rx.recv().await {
            if tx.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });
    fwd_tx
        .send(serde_json::to_string(&HarnessToPiMsg::Welcome).unwrap())
        .ok();

    let mut pi_rx = bridge.lock().await.pi_tx.subscribe();
    let f2 = fwd_tx.clone();
    tokio::spawn(async move {
        while let Ok(m) = pi_rx.recv().await {
            f2.send(m).ok();
        }
    });

    let dh = bridge.lock().await.dh.clone();
    while let Some(Ok(msg)) = rx.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };
        let p: PiToHarnessMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            _ => continue,
        };
        match p {
            PiToHarnessMsg::LaunchWebView { app_id, html } => dh.with_document(|d| {
                use autosurgeon::{hydrate, reconcile};
                let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                a.webviews.push(shared::LaunchedWebView {
                    id: app_id,
                    html,
                    status: WebViewStatus::Pending,
                });
                let mut t = d.transaction();
                let _ = reconcile(&mut t, &a);
                t.commit();
            }),
            PiToHarnessMsg::CloseWebView { app_id } => dh.with_document(|d| {
                use autosurgeon::{hydrate, reconcile};
                let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                a.webviews.retain(|w| w.id != app_id);
                a.intents.retain(|i| i.app_id != app_id);
                a.entities.retain(|e| e.app_id != app_id);
                let mut t = d.transaction();
                let _ = reconcile(&mut t, &a);
                t.commit();
            }),
            PiToHarnessMsg::EntityOp {
                op_id,
                kind,
                app_id,
                intent_type,
                entity_id,
                data,
                target_app,
            } => dh.with_document(|d| {
                use autosurgeon::{hydrate, reconcile};
                let mut a: AgentDoc = hydrate(d).unwrap_or_default();
                a.entity_ops.push(PendingEntityOp {
                    op_id,
                    kind,
                    app_id,
                    intent_type,
                    entity_id,
                    data,
                    target_app,
                });
                let mut t = d.transaction();
                let _ = reconcile(&mut t, &a);
                t.commit();
            }),
            PiToHarnessMsg::Exit => {
                dh.with_document(|d| {
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

fn kill_on(port: u16) {
    if let Ok(o) = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
    {
        for pid in String::from_utf8_lossy(&o.stdout).lines() {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .status();
        }
    }
}
