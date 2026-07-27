use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use samod::{ConnDirection, DocHandle};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowId};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use wry::{http, WebView};

use shared::{AgentDoc, Observation, RegisteredTool, WebViewStatus, SAMOD_WS_PORT};

const WEBMCP: &str = include_str!("webmcp.js");
const BRIDGE: &str = r#"(function(){var b=window.__a2web_bridge={};
b.syncTools=function(t){fetch('a2web://sync-tools',{method:'POST',body:JSON.stringify(t)}).catch(e=>console.error(e))};
b.sendObservation=function(o){fetch('a2web://send-observation',{method:'POST',body:JSON.stringify(o)}).catch(e=>console.error(e))};
window.__a2web=window.__a2web||{};
window.__a2web.syncTools=function(t){b.syncTools(t)};
window.__a2web.sendObservation=function(o){b.sendObservation(o)};
window.__a2web.onToolCall=async function(c,t,a,r){try{b.sendObservation(JSON.stringify({type:'tool_result',callId:c,result:r||await document.modelContext._executeTool(c,t,a)}),'tool_result')}catch(e){b.sendObservation(JSON.stringify({type:'tool_result',callId:c,error:e.message}),'tool_error')}};})();"#;

#[derive(Debug)]
enum HostEvent {
    WebAppLaunch {
        id: String,
        html: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: String,
        app_id: String,
    },
}

enum Cmd {
    Write(Box<dyn FnOnce(&mut AgentDoc) + Send>),
}

fn main() {
    let event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();

    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            let repo = samod::Repo::build_tokio().load().await;
            let doc_id = std::env::var("A2WEB_DOC_ID").expect("A2WEB_DOC_ID");
            let ws_url = std::env::var("A2WEB_WS_URL")
                .unwrap_or_else(|_| format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"));

            for i in 0..30 {
                if let Ok((stream, _)) = tokio_tungstenite::connect_async(&ws_url).await {
                    let _ = repo.connect_tungstenite(stream, ConnDirection::Outgoing);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let parsed: samod::DocumentId = doc_id.parse().expect("doc id");
            let handle = loop {
                if let Ok(Some(h)) = repo.find(parsed.clone()).await {
                    break h;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            };

            use futures::StreamExt;
            let mut changes = handle.changes();

            loop {
                tokio::select! {
                    Some(_) = changes.next() => {
                        handle.with_document(|doc| {
                            use autosurgeon::hydrate;
                            let agent: AgentDoc = hydrate(doc).unwrap_or_default();
                            for wv in &agent.webviews {
                                if wv.status == WebViewStatus::Pending {
                                    let _ = proxy.send_event(HostEvent::WebAppLaunch {
                                        id: wv.id.clone(), html: wv.html.clone(),
                                    });
                                }
                            }
                            for tc in &agent.tool_calls {
                                let _ = proxy.send_event(HostEvent::ToolCall {
                                    call_id: tc.id.clone(), tool_name: tc.tool_name.clone(),
                                    arguments: tc.arguments.clone(), app_id: tc.app_id.clone(),
                                });
                            }
                        });
                        handle.with_document(|doc| {
                            use autosurgeon::{hydrate, reconcile};
                            let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                            if !agent.tool_calls.is_empty() {
                                agent.tool_calls.clear();
                                let mut tx = doc.transaction();
                                let _ = reconcile(&mut tx, &agent);
                                tx.commit();
                            }
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        if let Cmd::Write(f) = cmd {
                            eprintln!("[web-host] write to doc");
                            handle.with_document(|doc| {
                                use autosurgeon::{hydrate, reconcile};
                                let mut agent: AgentDoc = hydrate(doc).unwrap_or_default();
                                f(&mut agent);
                                let mut tx = doc.transaction();
                                let _ = reconcile(&mut tx, &agent);
                                tx.commit();
                            });
                        }
                    }
                }
            }
        });
    });

    let mut views: HashMap<WindowId, (Window, WebView, String)> = HashMap::new();

    event_loop.run(move |event, wt, cf| {
        *cf = ControlFlow::Wait;
        match event {
            Event::UserEvent(HostEvent::WebAppLaunch { id, html }) => {
                let win = tao::window::WindowBuilder::new().with_title(&id).build(wt).unwrap();
                let wid = win.id();
                let escaped = serde_json::to_string(&id).unwrap_or_default();
                let app_id_for_proto = id.clone();
                let proto_tx = cmd_tx.clone();
                let launch_tx = cmd_tx.clone();
                let launch_id = id.clone();

                let wv = WebViewBuilder::new()
                    .with_asynchronous_custom_protocol("a2web".into(), move |_, req, resp| {
                        let uri = req.uri().to_string();
                        let body = String::from_utf8_lossy(req.body()).to_string();
                        let aid = app_id_for_proto.clone();
                        let ct = proto_tx.clone();

                        if uri.contains("sync-tools") {
                            if let Ok(tools) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                                let _ = ct.send(Cmd::Write(Box::new(move |agent| {
                                    let aid2 = aid.clone();
                                    agent.registered_tools.retain(|t| t.app_id != aid2);
                                    let aid_ref = &aid;
                                    for tv in &tools {
                                        let n = tv.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        if n.is_empty() { return; }
                                        agent.registered_tools.push(RegisteredTool {
                                            name: n,
                                            title: tv.get("title").and_then(|v| v.as_str()).map(String::from),
                                            description: tv.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                            input_schema: tv.get("inputSchema").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                            app_id: aid_ref.clone(), origin: "app".to_string(),
                                        });
                                    }
                                })));
                            }
                            resp.respond(http::Response::builder().status(200).body(Vec::new()).unwrap());
                        } else if uri.contains("send-observation") {
                            let _ = ct.send(Cmd::Write(Box::new(move |agent| {
                                let seq = agent.observations.len() as u64;
                                if let Ok(p) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&body) {
                                    let a = p.get("appId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let d = p.get("data").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let l = p.get("label").and_then(|v| v.as_str()).map(String::from);
                                    if !a.is_empty() && !d.is_empty() {
                                        agent.observations.push(Observation { app_id: a, data: d, label: l, sequence: seq });
                                    }
                                }
                            })));
                            resp.respond(http::Response::builder().status(200).body(Vec::new()).unwrap());
                        } else {
                            resp.respond(http::Response::builder().status(404).body(Vec::new()).unwrap());
                        }
                    })
                    .with_html(&format!(
                        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<script>window.__a2web={{appId:{}}};</script>
<script>{}</script><script>{}</script></head><body>{}</body></html>"#,
                        escaped, WEBMCP, BRIDGE, html
                    ))
                    .build(&win).unwrap();

                // Mark launched via channel (processed on tokio runtime -> immediate sync)
                let _ = launch_tx.send(Cmd::Write(Box::new(move |agent| {
                    for wv in &mut agent.webviews {
                        if wv.id == launch_id { wv.status = WebViewStatus::Launched; }
                    }
})));
                views.insert(wid, (win, wv, id));
            }

            Event::UserEvent(HostEvent::ToolCall { call_id, tool_name, arguments, app_id }) => {
                if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == app_id) {
                    let _ = wv.evaluate_script(&format!(
                        r#"(async()=>{{const r=await window.__a2web.onToolCall({:?},{:?},{});}})();"#,
                        call_id, tool_name, arguments
                    ));
                }
            }

            Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } => {
                if let Some((_, _, app_id)) = views.remove(&window_id) {
                    let _ = cmd_tx.send(Cmd::Write(Box::new(move |agent| {
                        agent.webviews.retain(|w| w.id != app_id);
                        agent.registered_tools.retain(|t| t.app_id != app_id);
                    })));
                }
            }

            Event::LoopDestroyed => {}
            _ => {}
        }
    });
}

use wry::WebViewBuilder;
