use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use samod::{ConnDirection, DocHandle};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowId};
use tokio::runtime::Runtime;
use wry::WebView;

use shared::{
    is_valid_entity_id, is_valid_intent_type, normalize_entity_data, Announcement,
    AnnouncementKind, AgentDoc, EntityResponse, PendingEntityOp, RegisteredEntity,
    RegisteredIntent, WebViewStatus, SAMOD_WS_PORT,
};

const WEBMCP: &str = include_str!("webmcp.js");
const BRIDGE: &str = r#"(function(){
var ipc = window.ipc, aid = window.__a2web.appId;
window.__a2web.registerIntent = function(i) { ipc.postMessage(JSON.stringify({type:'intent', appId:aid, intent:i})); };
window.__a2web.unregisterIntent = function(i) { ipc.postMessage(JSON.stringify({type:'intent-unregister', appId:aid, intent:i})); };
window.__a2web.registerEntity = function(e) { ipc.postMessage(JSON.stringify({type:'entity', appId:aid, entity:e})); };
window.__a2web.unregisterEntity = function(e) { ipc.postMessage(JSON.stringify({type:'entity-unregister', appId:aid, entity:e})); };
window.__a2web.sendEntityContent = function(opId, body) { ipc.postMessage(JSON.stringify({type:'entity-content', appId:aid, opId:opId, body:body})); };
})();"#;

#[derive(Debug)]
enum HostEvent {
    WebAppLaunch { id: String, html: String },
    /// A queued entity op from pi needs to be dispatched into a webview.
    EntityOp { op: PendingEntityOp },
    /// Content arrived from the source app of a transfer — deliver it to the
    /// target app now.
    EntityContentReady {
        op_id: String,
        intent_type: String,
        entity_id: String,
        data: String,
    },
}

// ── Document helpers ────────────────────────────────────────────────────

fn with_doc<T>(dh: &Mutex<DocHandle>, f: impl FnOnce(&mut AgentDoc) -> T) -> Option<T> {
    dh.lock().ok().and_then(|g| {
        Some(g.with_document(|doc| {
            use autosurgeon::{hydrate, reconcile};
            let mut a: AgentDoc = hydrate(doc).unwrap_or_default();
            let r = f(&mut a);
            let mut t = doc.transaction();
            let _ = reconcile(&mut t, &a);
            t.commit();
            r
        }))
    })
}

fn read_doc<T>(dh: &Mutex<DocHandle>, f: impl FnOnce(&AgentDoc) -> T) -> Option<T> {
    dh.lock().ok().and_then(|g| {
        Some(g.with_document(|doc| {
            use autosurgeon::hydrate;
            let a: AgentDoc = hydrate(doc).unwrap_or_default();
            f(&a)
        }))
    })
}

fn app_has_intent(dh: &Mutex<DocHandle>, app_id: &str, intent_type: &str) -> bool {
    read_doc(
        dh,
        |a| a.intents.iter().any(|i| i.app_id == app_id && i.intent_type == intent_type),
    )
    .unwrap_or(false)
}

fn app_has_entity(dh: &Mutex<DocHandle>, app_id: &str, intent_type: &str, entity_id: &str) -> bool {
    read_doc(
        dh,
        |a| {
            a.entities.iter().any(|e| {
                e.app_id == app_id && e.intent_type == intent_type && e.entity_id == entity_id
            })
        },
    )
    .unwrap_or(false)
}

fn write_response(dh: &Mutex<DocHandle>, op: &PendingEntityOp, data: Option<String>) {
    with_doc(dh, |a| {
        a.entity_responses.push(EntityResponse {
            op_id: op.op_id.clone(),
            kind: op.kind.clone(),
            app_id: op.app_id.clone(),
            intent_type: op.intent_type.clone(),
            entity_id: op.entity_id.clone(),
            data,
            is_error: false,
            error: None,
        });
    });
}

fn write_error(dh: &Mutex<DocHandle>, op: &PendingEntityOp, error: &str) {
    with_doc(dh, |a| {
        a.entity_responses.push(EntityResponse {
            op_id: op.op_id.clone(),
            kind: op.kind.clone(),
            app_id: op.app_id.clone(),
            intent_type: op.intent_type.clone(),
            entity_id: op.entity_id.clone(),
            data: None,
            is_error: true,
            error: Some(error.to_string()),
        });
    });
}

/// Announce a registration change to the agent context. Registered
/// announcements are deduped — an intent/entity is announced only the first
/// time it appears; later refreshes stay silent.
fn push_announcement(
    dh: &Mutex<DocHandle>,
    kind: AnnouncementKind,
    app_id: &str,
    intent_type: &str,
    entity_id: Option<&str>,
) {
    with_doc(dh, |a| {
        let dup = match kind {
            AnnouncementKind::IntentRegistered => a
                .intents
                .iter()
                .any(|i| i.app_id == app_id && i.intent_type == intent_type),
            AnnouncementKind::EntityRegistered => a.entities.iter().any(|e| {
                e.app_id == app_id
                    && e.intent_type == intent_type
                    && e.entity_id == entity_id.unwrap_or("")
            }),
            _ => false,
        };
        if dup {
            return;
        }
        // Keep the system-side registry in sync with what gets announced.
        match kind {
            AnnouncementKind::IntentRegistered => {
                a.intents.push(RegisteredIntent {
                    app_id: app_id.to_string(),
                    intent_type: intent_type.to_string(),
                });
            }
            AnnouncementKind::IntentUnregistered => {
                a.intents.retain(|i| !(i.app_id == app_id && i.intent_type == intent_type));
            }
            AnnouncementKind::EntityRegistered => {
                if let Some(eid) = entity_id {
                    a.entities.push(RegisteredEntity {
                        app_id: app_id.to_string(),
                        intent_type: intent_type.to_string(),
                        entity_id: eid.to_string(),
                    });
                }
            }
            AnnouncementKind::EntityUnregistered => {
                if let Some(eid) = entity_id {
                    a.entities.retain(|e| {
                        !(e.app_id == app_id && e.intent_type == intent_type && e.entity_id == eid)
                    });
                }
            }
        }
        a.announcements.push(Announcement {
            kind,
            app_id: app_id.to_string(),
            intent_type: intent_type.to_string(),
            entity_id: entity_id.map(String::from),
        });
    });
}

fn main() {
    let event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (doc_tx, doc_rx) = std::sync::mpsc::channel::<DocHandle>();

    let pending_launches: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let pending_ops: Arc<Mutex<HashMap<String, PendingEntityOp>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let pl = pending_launches.clone();
    let ppo = pending_ops.clone();
    let pproxy = proxy.clone();
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            let repo = samod::Repo::build_tokio().load().await;
            let doc_id = std::env::var("A2WEB_DOC_ID").expect("A2WEB_DOC_ID");
            let ws_url = std::env::var("A2WEB_WS_URL")
                .unwrap_or_else(|_| format!("ws://127.0.0.1:{SAMOD_WS_PORT}/sync"));

            for _i in 0..30 {
                if let Ok((s, _)) = tokio_tungstenite::connect_async(&ws_url).await {
                    let _ = repo.connect_tungstenite(s, ConnDirection::Outgoing);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let pid: samod::DocumentId = doc_id.parse().expect("doc id");
            let handle = loop {
                if let Ok(Some(h)) = repo.find(pid.clone()).await {
                    break h;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            };

            doc_tx.send(handle.clone()).ok();

            use futures::StreamExt;
            let mut changes = handle.changes();

            // Scan the current document state, then rescan on every change.
            // The initial scan matters: ops/launches written before this
            // web-host connected to the CRDT will not replay through
            // `changes` (it only emits future changes).
            let scan = || {
                // 1) Launch pending webviews
                handle.with_document(|doc| {
                    use autosurgeon::hydrate;
                    let a: AgentDoc = hydrate(doc).unwrap_or_default();
                    let mut pl_guard = pl.lock().unwrap();
                    for w in &a.webviews {
                        if w.status == WebViewStatus::Pending && !pl_guard.contains(&w.id) {
                            pl_guard.insert(w.id.clone());
                            let _ = pproxy.send_event(HostEvent::WebAppLaunch {
                                id: w.id.clone(),
                                html: w.html.clone(),
                            });
                        }
                    }
                });

                // 2) Dispatch queued entity ops (skip ones already pending)
                let ops: Vec<PendingEntityOp> = handle.with_document(|doc| {
                    use autosurgeon::hydrate;
                    let a: AgentDoc = hydrate(doc).unwrap_or_default();
                    let pending = ppo.lock().unwrap();
                    a.entity_ops
                        .iter()
                        .filter(|o| !pending.contains_key(&o.op_id))
                        .cloned()
                        .collect()
                });
                if !ops.is_empty() {
                    let ids: HashSet<String> = ops.iter().map(|o| o.op_id.clone()).collect();
                    {
                        let mut pending = ppo.lock().unwrap();
                        for op in &ops {
                            pending.insert(op.op_id.clone(), op.clone());
                        }
                    }
                    for op in &ops {
                        let _ = pproxy.send_event(HostEvent::EntityOp { op: op.clone() });
                    }
                    // Remove dispatched ops from the queue so they don't
                    // accumulate (the pending_ops map guards re-dispatch).
                    handle.with_document(|doc| {
                        use autosurgeon::{hydrate, reconcile};
                        let mut a: AgentDoc = hydrate(doc).unwrap_or_default();
                        let before = a.entity_ops.len();
                        a.entity_ops.retain(|o| !ids.contains(&o.op_id));
                        if a.entity_ops.len() != before {
                            let mut t = doc.transaction();
                            let _ = reconcile(&mut t, &a);
                            t.commit();
                        }
                    });
                }
            };
            scan();
            while let Some(_) = changes.next().await {
                scan();
            }
        });
    });

    let dh = doc_rx.recv().expect("doc handle");
    let dh = Arc::new(Mutex::new(dh));
    let mut views: HashMap<WindowId, (Window, WebView, String)> = HashMap::new();

    event_loop.run(move |event, wt, cf| {
        *cf = ControlFlow::Wait;
        match event {
            Event::UserEvent(HostEvent::WebAppLaunch { id, html }) => {
                let win = tao::window::WindowBuilder::new().with_title(&id).build(wt).unwrap();
                let wid = win.id();

                let wv = WebViewBuilder::new()
                    .with_ipc_handler({
                        let dh = dh.clone();
                        let proxy = proxy.clone();
                        let pending_ops = pending_ops.clone();
                        move |req| {
                            let body = req.body().to_string();
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                                let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let aid = v.get("appId").and_then(|x| x.as_str()).unwrap_or("webapp").to_string();
                                match typ.as_str() {
                                    // App registered an intent capability: {type}
                                    "intent" => {
                                        if let Some(intent) = v.get("intent") {
                                            let itype = intent.get("type").and_then(|x| x.as_str()).unwrap_or("");
                                            if is_valid_intent_type(itype) {
                                                let exists = read_doc(&dh, |a| {
                                                    a.intents.iter().any(|i| {
                                                        i.app_id == aid && i.intent_type == itype
                                                    })
                                                })
                                                .unwrap_or(false);
                                                if !exists {
                                                    push_announcement(
                                                        &dh,
                                                        AnnouncementKind::IntentRegistered,
                                                        &aid,
                                                        itype,
                                                        None,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    "intent-unregister" => {
                                        if let Some(intent) = v.get("intent") {
                                            let itype = intent.get("type").and_then(|x| x.as_str()).unwrap_or("");
                                            push_announcement(
                                                &dh,
                                                AnnouncementKind::IntentUnregistered,
                                                &aid,
                                                itype,
                                                None,
                                            );
                                        }
                                    }
                                    // App registered an entity: {type, id}
                                    "entity" => {
                                        if let Some(entity) = v.get("entity") {
                                            let itype = entity.get("type").and_then(|x| x.as_str()).unwrap_or("");
                                            let eid = entity.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                            if is_valid_intent_type(itype)
                                                && is_valid_entity_id(eid)
                                                && app_has_intent(&dh, &aid, itype)
                                            {
                                                let exists = read_doc(&dh, |a| {
                                                    a.entities.iter().any(|e| {
                                                        e.app_id == aid
                                                            && e.intent_type == itype
                                                            && e.entity_id == eid
                                                    })
                                                })
                                                .unwrap_or(false);
                                                if !exists {
                                                    push_announcement(
                                                        &dh,
                                                        AnnouncementKind::EntityRegistered,
                                                        &aid,
                                                        itype,
                                                        Some(eid),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    "entity-unregister" => {
                                        if let Some(entity) = v.get("entity") {
                                            let itype = entity.get("type").and_then(|x| x.as_str()).unwrap_or("");
                                            let eid = entity.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                            push_announcement(
                                                &dh,
                                                AnnouncementKind::EntityUnregistered,
                                                &aid,
                                                itype,
                                                Some(eid),
                                            );
                                        }
                                    }
                                    // App answered an 'entity-request' with content.
                                    "entity-content" => {
                                        let op_id = v.get("opId").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                        if op_id.is_empty() {
                                            return;
                                        }
                                        let body = v.get("body").cloned().unwrap_or(serde_json::Value::Null);
                                        let itype = body.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                        let entities = body
                                            .get("entities")
                                            .and_then(|x| x.as_array())
                                            .cloned()
                                            .unwrap_or_default();
                                        let pending = pending_ops.lock().unwrap().get(&op_id).cloned();
                                        if let Some(op) = pending {
                                            // Normalize each entity's content against the scheme.
                                            let mut normed: Vec<serde_json::Value> = Vec::new();
                                            let mut bad = false;
                                            for e in &entities {
                                                let eid = e.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                                let data = e.get("data").cloned().unwrap_or(serde_json::Value::Null);
                                                match normalize_entity_data(eid, &data) {
                                                    Some(n) => normed.push(n),
                                                    None => { bad = true; break; }
                                                }
                                            }
                                            if entities.is_empty() && op.kind != "list" {
                                                bad = true;
                                            }
                                            match op.kind.as_str() {
                                                "list" => {
                                                    if bad {
                                                        write_error(&dh, &op, "invalid entity content for intent scheme");
                                                    } else {
                                                        let arr = serde_json::Value::Array(normed);
                                                        write_response(&dh, &op, Some(arr.to_string()));
                                                    }
                                                    pending_ops.lock().unwrap().remove(&op_id);
                                                }
                                                "read" => {
                                                    if bad || normed.is_empty() {
                                                        write_error(&dh, &op, "invalid entity content for intent scheme");
                                                    } else {
                                                        write_response(&dh, &op, Some(normed[0].to_string()));
                                                    }
                                                    pending_ops.lock().unwrap().remove(&op_id);
                                                }
                                                "transfer" => {
                                                    if bad || normed.is_empty() {
                                                        write_error(&dh, &op, "invalid entity content for intent scheme");
                                                        pending_ops.lock().unwrap().remove(&op_id);
                                                    } else {
                                                        let entity_id = normed[0]
                                                            .get("id")
                                                            .and_then(|x| x.as_str())
                                                            .unwrap_or("")
                                                            .to_string();
                                                        let _ = proxy.send_event(HostEvent::EntityContentReady {
                                                            op_id: op_id.clone(),
                                                            intent_type: itype,
                                                            entity_id,
                                                            data: normed[0].to_string(),
                                                        });
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    })
                    .with_html(&format!(
                        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<script>window.__a2web={{appId:{}}};</script>
<script>{}</script><script>{}</script></head><body>{}</body></html>"#,
                        serde_json::to_string(&id).unwrap_or_default(), WEBMCP, BRIDGE, html
                    ))
                    .build(&win).unwrap();

                if let Ok(g) = dh.lock() {
                    g.with_document(|doc| {
                        use autosurgeon::{hydrate, reconcile};
                        let mut a: AgentDoc = hydrate(doc).unwrap_or_default();
                        for w in &mut a.webviews {
                            if w.id == id {
                                w.status = WebViewStatus::Launched;
                            }
                        }
                        let mut t = doc.transaction();
                        let _ = reconcile(&mut t, &a);
                        t.commit();
                    });
                }
                pending_launches.lock().unwrap().remove(&id);
                views.insert(wid, (win, wv, id));
            }

            Event::UserEvent(HostEvent::EntityOp { op }) => {
                match op.kind.as_str() {
                    "list" => {
                        if !app_has_intent(&dh, &op.app_id, &op.intent_type) {
                            write_error(&dh, &op, "app has no such intent");
                            pending_ops.lock().unwrap().remove(&op.op_id);
                            return;
                        }
                        if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == op.app_id) {
                            let _ = wv.evaluate_script(&format!(
                                r#"document.dispatchEvent(new CustomEvent('entity-request', {{ detail: {{ requestId: {:?}, type: {:?}, id: "", mode: "list" }} }}));"#,
                                op.op_id, op.intent_type
                            ));
                        } else {
                            write_error(&dh, &op, "app window not found");
                            pending_ops.lock().unwrap().remove(&op.op_id);
                        }
                    }
                    "read" | "transfer" => {
                        if !app_has_entity(&dh, &op.app_id, &op.intent_type, &op.entity_id) {
                            write_error(&dh, &op, "no entity with this id is registered in the app");
                            pending_ops.lock().unwrap().remove(&op.op_id);
                            return;
                        }
                        if op.kind == "transfer" {
                            let target = op.target_app.clone().unwrap_or_default();
                            if !app_has_intent(&dh, &target, &op.intent_type) {
                                write_error(
                                    &dh,
                                    &op,
                                    &format!("target app '{}' does not handle intent '{}'", target, op.intent_type),
                                );
                                pending_ops.lock().unwrap().remove(&op.op_id);
                                return;
                            }
                        }
                        if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == op.app_id) {
                            let _ = wv.evaluate_script(&format!(
                                r#"document.dispatchEvent(new CustomEvent('entity-request', {{ detail: {{ requestId: {:?}, type: {:?}, id: {:?}, mode: {:?} }} }}));"#,
                                op.op_id, op.intent_type, op.entity_id, op.kind
                            ));
                        } else {
                            write_error(&dh, &op, "app window not found");
                            pending_ops.lock().unwrap().remove(&op.op_id);
                        }
                    }
                    "set" => {
                        if !app_has_entity(&dh, &op.app_id, &op.intent_type, &op.entity_id) {
                            write_error(&dh, &op, "no entity with this id is registered in the app");
                            pending_ops.lock().unwrap().remove(&op.op_id);
                            return;
                        }
                        let data = op
                            .data
                            .as_deref()
                            .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok());
                        let norm = data.as_ref().and_then(|d| normalize_entity_data(&op.entity_id, d));
                        match norm {
                            Some(content) => {
                                if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == op.app_id) {
                                    let data_json = content.to_string();
                                    let _ = wv.evaluate_script(&format!(
                                        r#"document.dispatchEvent(new CustomEvent('entity-receive', {{ detail: {{ type: {:?}, id: {:?}, data: {}, source: {:?} }} }}));"#,
                                        op.intent_type, op.entity_id, data_json, "agent"
                                    ));
                                    write_response(&dh, &op, None);
                                } else {
                                    write_error(&dh, &op, "app window not found");
                                }
                            }
                            None => {
                                write_error(&dh, &op, "invalid entity content for intent scheme");
                            }
                        }
                        pending_ops.lock().unwrap().remove(&op.op_id);
                    }
                    _ => {
                        write_error(&dh, &op, "unknown entity op kind");
                        pending_ops.lock().unwrap().remove(&op.op_id);
                    }
                }
            }

            Event::UserEvent(HostEvent::EntityContentReady { op_id, intent_type, entity_id, data }) => {
                // Content from the source app of a transfer. Clone it into the
                // target app, announce the cloned entity to the agent context,
                // and resolve the op.
                let pending = pending_ops.lock().unwrap().get(&op_id).cloned();
                if let Some(op) = pending {
                    let target = op.target_app.clone().unwrap_or_default();
                    if let Some((_, wv, _)) = views.values().find(|(_, _, a)| *a == target) {
                        let _ = wv.evaluate_script(&format!(
                            r#"document.dispatchEvent(new CustomEvent('entity-receive', {{ detail: {{ type: {:?}, id: {:?}, data: {}, source: {:?} }} }}));"#,
                            intent_type, entity_id, data, "transfer"
                        ));
                        // The cloned entity now lives in the target app —
                        // announce it (type + id only, content stays app-side).
                        push_announcement(
                            &dh,
                            AnnouncementKind::EntityRegistered,
                            &target,
                            &intent_type,
                            Some(&entity_id),
                        );
                        write_response(&dh, &op, None);
                    } else {
                        write_error(&dh, &op, "target app window not found");
                    }
                    pending_ops.lock().unwrap().remove(&op_id);
                }
            }

            Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } => {
                if let Some((_, _, app_id)) = views.remove(&window_id) {
                    with_doc(&dh, |a| {
                        a.webviews.retain(|w| w.id != app_id);
                        a.intents.retain(|i| i.app_id != app_id);
                        a.entities.retain(|e| e.app_id != app_id);
                    });
                    // Fail any pending ops for this app so the agent isn't left hanging.
                    let mut pending = pending_ops.lock().unwrap();
                    let to_fail: Vec<PendingEntityOp> = pending
                        .iter()
                        .filter(|(_, op)| op.app_id == app_id || op.target_app.as_deref() == Some(&app_id))
                        .map(|(_, op)| op.clone())
                        .collect();
                    for op in to_fail {
                        write_error(&dh, &op, "app closed");
                        pending.remove(&op.op_id);
                    }
                }
            }

            Event::LoopDestroyed => {}
            _ => {}
        }
    });
}

use wry::WebViewBuilder;
