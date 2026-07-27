use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use once_cell::sync::OnceCell;
use samod::{ConnDirection, DocHandle};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowId};
use tokio::runtime::Runtime;
use wry::WebView;

use shared::{AgentDoc, Observation, RegisteredTool, WebViewStatus, SAMOD_WS_PORT};

static DOC: OnceCell<Mutex<DocHandle>> = OnceCell::new();
const WEBMCP: &str = include_str!("webmcp.js");
const BRIDGE: &str = r#"(function(){
var ipc = window.ipc, aid = window.__a2web.appId;
window.__a2web.sendObservation = function(o) { ipc.postMessage(JSON.stringify({type:'obs',appId:aid,data:o})); };
window.__a2web.syncTools = function(t) { ipc.postMessage(JSON.stringify({type:'tools',appId:aid,tools:t})); };
window.__a2web.onToolCall = async function(c,t,a,r) {
  try { ipc.postMessage(JSON.stringify({type:'result',appId:aid,callId:c,result:r||await document.modelContext._executeTool(c,t,a)})); }
  catch(e) { ipc.postMessage(JSON.stringify({type:'result',appId:aid,callId:c,error:e.message})); }
};
})();"#;

#[derive(Debug)]
enum HostEvent {
    WebAppLaunch { id: String, html: String },
    ToolCall { call_id: String, tool_name: String, arguments: String, app_id: String },
}

fn with_doc<F: FnOnce(&mut AgentDoc)>(f: F) {
    eprintln!("[web-host] with_doc: DOC={}", DOC.get().is_some());
    if let Some(d) = DOC.get() {
        if let Ok(g) = d.lock() {
            g.with_document(|doc| {
                use autosurgeon::{hydrate, reconcile};
                let mut a: AgentDoc = hydrate(doc).unwrap_or_default();
                f(&mut a);
                let mut t = doc.transaction();
                let _ = reconcile(&mut t, &a);
                t.commit();
            });
            eprintln!("[web-host] write done");
        }
    } else {
        eprintln!("[web-host] DOC not set!");
    }
}

fn main() {
    let event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            let repo = samod::Repo::build_tokio().load().await;
            let doc_id = std::env::var("A2WEB_DOC_ID").expect("A2WEB_DOC_ID");
            let ws_url = std::env::var("A2WEB_WS_URL")
                .unwrap_or_else(|_| format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"));

            for i in 0..30 {
                if let Ok((s, _)) = tokio_tungstenite::connect_async(&ws_url).await {
                    let _ = repo.connect_tungstenite(s, ConnDirection::Outgoing);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let pid: samod::DocumentId = doc_id.parse().expect("doc id");
            let handle = loop {
                if let Ok(Some(h)) = repo.find(pid.clone()).await { break h; }
                tokio::time::sleep(Duration::from_millis(500)).await;
            };
            DOC.set(Mutex::new(handle.clone())).ok();

            use futures::StreamExt;
            let mut changes = handle.changes();
            while let Some(_) = changes.next().await {
                handle.with_document(|doc| {
                    use autosurgeon::hydrate;
                    let a: AgentDoc = hydrate(doc).unwrap_or_default();
                    for w in &a.webviews { if w.status == WebViewStatus::Pending {
                        let _ = proxy.send_event(HostEvent::WebAppLaunch { id: w.id.clone(), html: w.html.clone() });
                    }}
                    for tc in &a.tool_calls {
                        let _ = proxy.send_event(HostEvent::ToolCall {
                            call_id: tc.id.clone(), tool_name: tc.tool_name.clone(),
                            arguments: tc.arguments.clone(), app_id: tc.app_id.clone(),
                        });
                    }
                });
                handle.with_document(|doc| {
                    use autosurgeon::{hydrate, reconcile};
                    let mut a: AgentDoc = hydrate(doc).unwrap_or_default();
                    if !a.tool_calls.is_empty() { a.tool_calls.clear();
                        let mut t = doc.transaction();
                        let _ = reconcile(&mut t, &a);
                        t.commit();
                    }
                });
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

                let wv = WebViewBuilder::new()
                    .with_ipc_handler(move |req| {
                        let body = req.body().to_string();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                            let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
                            let aid = v.get("appId").and_then(|x| x.as_str()).unwrap_or("webapp").to_string();
                            with_doc(|a| match typ {
                                "tools" => {
                                    if let Some(tools) = v.get("tools").and_then(|x| x.as_array()) {
                                        a.registered_tools.retain(|t| t.app_id != aid);
                                        for tv in tools {
                                            let n = tv.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                            if n.is_empty() { continue; }
                                            a.registered_tools.push(RegisteredTool {
                                                name: n,
                                                title: tv.get("title").and_then(|x| x.as_str()).map(String::from),
                                                description: tv.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                                input_schema: tv.get("inputSchema").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                                app_id: aid.clone(), origin: "app".to_string(),
                                            });
                                        }
                                    }
                                }
                                "obs" => {
                                    let seq = a.observations.len() as u64;
                                    if let Some(data) = v.get("data") {
                                        let d = data.get("data").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                        let l = data.get("label").and_then(|x| x.as_str()).map(String::from);
                                        if !d.is_empty() { a.observations.push(Observation { app_id: aid, data: d, label: l, sequence: seq }); }
                                    }
                                }
                                "result" => {
                                    let seq = a.observations.len() as u64;
                                    let data = serde_json::to_string(&v).unwrap_or_default();
                                    a.observations.push(Observation { app_id: aid, data, label: Some("tool_result".into()), sequence: seq });
                                }
                                _ => {}
                            });
                        }
                    })
                    .with_html(&format!(
                        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<script>window.__a2web={{appId:{}}};</script>
<script>{}</script><script>{}</script></head><body>{}</body></html>"#,
                        serde_json::to_string(&id).unwrap_or_default(), WEBMCP, BRIDGE, html
                    ))
                    .build(&win).unwrap();

                with_doc(|a| { for w in &mut a.webviews { if w.id == id { w.status = WebViewStatus::Launched; } } });
                views.insert(wid, (win, wv, id));
            }

            Event::UserEvent(HostEvent::ToolCall { call_id, tool_name, arguments, app_id }) => {
                if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == app_id) {
                    let _ = wv.evaluate_script(&format!(
                        r#"(async()=>{{await window.__a2web.onToolCall({:?},{:?},{});}})();"#,
                        call_id, tool_name, arguments
                    ));
                }
            }

            Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } => {
                if let Some((_, _, app_id)) = views.remove(&window_id) {
                    with_doc(|a| { a.webviews.retain(|w| w.id != app_id); a.registered_tools.retain(|t| t.app_id != app_id); });
                }
            }

            Event::LoopDestroyed => {}
            _ => {}
        }
    });
}

use wry::WebViewBuilder;
