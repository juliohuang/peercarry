use std::sync::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::Path as AxumPath,
    http::{header, Response as HttpResponse},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use peercarry_core::{
    config::Config,
    engine::Engine,
    model::{Entry, EntryKind, Origin},
    server::AppState,
    store::Store,
    transfer_jobs,
};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fixture() -> (PathBuf, AppState) {
    let root = std::env::temp_dir().join(format!("peercarry-transfer-jobs-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let config = Arc::new(Config::default());
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "transfer-test".into(),
        node_id: "transfer-test".into(),
        addr: "http://127.0.0.1:1".into(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    (
        root,
        AppState {
            config,
            store,
            origin,
            engine,
        },
    )
}

async fn start(app: AppState, root: PathBuf) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = transfer_jobs::router_with_dir(app.clone(), root).with_state(app);
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn recovery_preserves_running_jobs_and_local_gate_rejects_bad_host() {
    let _guard = ENV_LOCK.lock().await;
    let (root, app) = fixture();
    let jobs = root.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let record = json!({
        "jobs": [{"id":"test-job","operation":"download","entry_id":"source-entry","node_id":null,"target_name":"source","entry_kind":"files","status":"running","created_at":"2026-09-26T00:00:00Z","updated_at":"2026-09-26T00:00:00Z","expires_at":"2026-09-27T00:00:00Z","error":null,"files":[],"retry_at":null,"attempts":1,"generation":1}],
        "entries": {}
    });
    std::fs::write(jobs.join("jobs.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    let (url, task) = start(app, jobs).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let rows: Value = client
        .get(format!("{url}/v1/transfers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows[0]["status"], "paused");
    let rejected = client
        .get(format!("{url}/v1/transfers"))
        .header("Host", "example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let unknown = client
        .post(format!("{url}/v1/transfers"))
        .json(&json!({"operation":"send","node_id":"x","extra":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    task.abort();
    let _ = task.await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn corrupt_queue_fails_closed_without_overwriting_original_bytes() {
    let _guard = ENV_LOCK.lock().await;
    let (root, app) = fixture();
    let jobs = root.join("corrupt-jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let original = b"not valid json";
    std::fs::write(jobs.join("jobs.json"), original).unwrap();
    let (url, task) = start(app, jobs.clone()).await;
    let response = Client::new()
        .get(format!("{url}/v1/transfers"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(std::fs::read(jobs.join("jobs.json")).unwrap(), original);
    task.abort();
    let _ = task.await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn cancel_retry_and_expiry_transitions_are_durable_and_bounded() {
    let _guard = ENV_LOCK.lock().await;
    let (root, app) = fixture();
    let jobs = root.join("transitions");
    std::fs::create_dir_all(&jobs).unwrap();
    let now = chrono::Utc::now();
    let record = json!({"jobs":[
        {"id":"cancel-me","operation":"send","entry_id":"e1","node_id":"peer","target_name":"peer","entry_kind":"text","status":"waiting_for_peer","created_at":now,"updated_at":now,"expires_at":now+chrono::Duration::hours(1),"error":"offline","files":[],"retry_at":"2099-01-01T00:00:00Z","attempts":2,"generation":3},
        {"id":"retry-me","operation":"send","entry_id":"e2","node_id":"peer","target_name":"peer","entry_kind":"text","status":"failed","created_at":now,"updated_at":now,"expires_at":now+chrono::Duration::hours(1),"error":"manual failure","files":[],"retry_at":null,"attempts":1,"generation":4},
        {"id":"expire-me","operation":"download","entry_id":"e3","node_id":null,"target_name":"peer","entry_kind":"files","status":"waiting_for_peer","created_at":now-chrono::Duration::hours(2),"updated_at":now-chrono::Duration::hours(2),"expires_at":now-chrono::Duration::hours(1),"error":"offline","files":[],"retry_at":null,"attempts":1,"generation":1}
    ],"entries":{}});
    std::fs::write(jobs.join("jobs.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    let (url, task) = start(app.clone(), jobs.clone()).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let cancel: Value = client
        .post(format!("{url}/v1/transfers/cancel-me/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancel["status"], "cancelled");
    let retry: Value = client
        .post(format!("{url}/v1/transfers/retry-me/retry"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["status"], "queued");
    assert_eq!(retry["generation"], 5);
    tokio::time::sleep(std::time::Duration::from_millis(650)).await;
    let rows: Value = client
        .get(format!("{url}/v1/transfers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let status = |id: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == id)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(status("cancel-me"), "cancelled");
    assert_eq!(status("expire-me"), "expired");
    task.abort();
    let _ = task.await;
    let (url, task) = start(app, jobs.clone()).await;
    let recovered: Value = client
        .get(format!("{url}/v1/transfers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        recovered
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == "cancel-me")
            .unwrap()["status"],
        "cancelled"
    );
    task.abort();
    let _ = task.await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn queued_send_waits_for_exact_offline_peer_then_pushes_saved_entry_once() {
    let _guard = ENV_LOCK.lock().await;
    let (root, mut app) = fixture();
    let pushed = Arc::new(Mutex::new(Vec::<Entry>::new()));
    let pushed_ref = pushed.clone();
    let peer_router=Router::new()
        .route("/v1/hello",get(||async{Json(json!({"protocol":peercarry_core::protocol::PROTOCOL_VERSION,"host":"mock-peer","node_id":"peer-1","os":"windows","version":"test","capabilities":[]}))}))
        .route("/v1/entries",post(move |Json(entry):Json<Entry>|{let pushed=pushed_ref.clone();async move{pushed.lock().unwrap().push(entry);StatusCode::OK}}));
    let peer_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_addr = peer_listener.local_addr().unwrap();
    let peer_task =
        tokio::spawn(async move { axum::serve(peer_listener, peer_router).await.unwrap() });
    Arc::make_mut(&mut app.config).network.port = peer_addr.port();
    let entry = Entry {
        id: "fixed-entry".into(),
        kind: EntryKind::Text,
        origin: app.origin.clone(),
        created_at: Utc::now(),
        text: Some("saved payload".into()),
        blob: None,
        thumb: None,
        files: None,
        preview: "saved payload".into(),
        pinned: false,
        size: 13,
        holder: None,
    };
    app.store.insert(&entry).unwrap();
    let status_file = root.join("tailnet-status.json");
    let script = root.join("fake-tailscale.cmd");
    std::fs::write(
        &script,
        format!("@echo off\r\ntype \"{}\"\r\n", status_file.display()),
    )
    .unwrap();
    std::env::set_var("PEERCARRY_TAILSCALE", &script);
    let write_status = |online: bool| {
        let value = json!({"Self":{"ID":"local","HostName":"local","OS":"windows","TailscaleIPs":["127.0.0.1"]},"Peer":{"peer-1":{"ID":"peer-1","HostName":"mock-peer","DNSName":"mock-peer.tailnet.","OS":"windows","TailscaleIPs":["127.0.0.1"],"Online":online}}});
        std::fs::write(&status_file, serde_json::to_vec(&value).unwrap()).unwrap();
    };
    write_status(false);
    let (url, task) = start(app, jobs_path(&root)).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let created: Value = client
        .post(format!("{url}/v1/transfers"))
        .json(&json!({"operation":"send","node_id":"peer-1","entry_id":"fixed-entry"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let waiting: Value = client
        .get(format!("{url}/v1/transfers/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(waiting["status"], "waiting_for_peer");
    write_status(true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let item: Value = client
            .get(format!("{url}/v1/transfers/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if item["status"] == "completed" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "send did not finish: {item}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    {
        let records = pushed.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].text.as_deref(), Some("saved payload"));
    }
    task.abort();
    let _ = task.await;
    peer_task.abort();
    let _ = peer_task.await;
    std::env::remove_var("PEERCARRY_TAILSCALE");
    let _ = std::fs::remove_dir_all(root);
}

fn jobs_path(root: &Path) -> PathBuf {
    root.join("transfer-jobs")
}

#[tokio::test]
async fn download_retry_reuses_completed_file_without_writing_clipboard() {
    let _guard = ENV_LOCK.lock().await;
    let root = std::env::temp_dir().join(format!("peercarry-transfer-download-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let failed = Arc::new(AtomicBool::new(true));
    let counts = Arc::new(Mutex::new(vec![0usize, 0usize]));
    let entry = Entry {
        id: "remote-files".into(),
        kind: EntryKind::Files,
        origin: Origin {
            host: "mock-source".into(),
            node_id: "peer-download".into(),
            addr: "http://127.0.0.1:9".into(),
            os: "windows".into(),
        },
        created_at: Utc::now(),
        text: None,
        blob: None,
        thumb: None,
        files: Some(vec![
            peercarry_core::model::FileRef {
                path: "C:\\untrusted\\one.txt".into(),
                name: "one.txt".into(),
                size: 3,
                is_dir: false,
                hash: None,
            },
            peercarry_core::model::FileRef {
                path: "C:\\untrusted\\two.txt".into(),
                name: "two.txt".into(),
                size: 3,
                is_dir: false,
                hash: None,
            },
        ]),
        preview: "2 files".into(),
        pinned: false,
        size: 6,
        holder: None,
    };
    let list_entry = entry.clone();
    let files_fail = failed.clone();
    let file_counts = counts.clone();
    let peer=Router::new()
        .route("/v1/hello",get(||async{Json(json!({"protocol":peercarry_core::protocol::PROTOCOL_VERSION,"host":"mock-source","node_id":"peer-download","os":"windows","version":"test","capabilities":[]}))}))
        .route("/v1/entries",get(move ||{let entry=list_entry.clone();async move{Json(vec![entry])}}))
        .route("/v1/entries/{id}/files/{index}",get(move |AxumPath((_id,index)):AxumPath<(String,usize)>|{let fail=files_fail.clone();let counts=file_counts.clone();async move{counts.lock().unwrap()[index]+=1;if index==1&&fail.load(Ordering::SeqCst){return StatusCode::NOT_FOUND.into_response()}HttpResponse::builder().status(StatusCode::OK).header(peercarry_core::protocol::FILENAME_HEADER,if index==0{"one.txt"}else{"two.txt"}).header(peercarry_core::protocol::IS_DIR_HEADER,"0").header(header::CONTENT_LENGTH,3).body(Body::from(if index==0{&b"one"[..]}else{&b"two"[..]})).unwrap().into_response()}}));
    let peer_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_addr = peer_listener.local_addr().unwrap();
    let peer_task = tokio::spawn(async move { axum::serve(peer_listener, peer).await.unwrap() });
    let status_file = root.join("status.json");
    let script = root.join("fake-tailscale.cmd");
    std::fs::write(
        &script,
        format!("@echo off\r\ntype \"{}\"\r\n", status_file.display()),
    )
    .unwrap();
    std::env::set_var("PEERCARRY_TAILSCALE", &script);
    let status = json!({"Self":{"ID":"local","HostName":"local","OS":"windows","TailscaleIPs":["127.0.0.1"]},"Peer":{"peer-download":{"ID":"peer-download","HostName":"mock-source","OS":"windows","TailscaleIPs":["127.0.0.1"],"Online":true}}});
    std::fs::write(&status_file, serde_json::to_vec(&status).unwrap()).unwrap();
    let mut cfg = Config::default();
    cfg.network.port = peer_addr.port();
    cfg.storage.download_dir = Some(root.join("downloads"));
    let config = Arc::new(cfg);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    store.insert(&entry).unwrap();
    let origin = Origin {
        host: "local".into(),
        node_id: "local-node".into(),
        addr: "http://127.0.0.1".into(),
        os: "windows".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = AppState {
        config,
        store,
        origin,
        engine,
    };
    let (url, task) = start(app, jobs_path(&root)).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let created: Value = client
        .post(format!("{url}/v1/transfers"))
        .json(&json!({"operation":"download","entry_id":"remote-files"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let job: Value = client
            .get(format!("{url}/v1/transfers/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if job["status"] == "failed" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "download did not fail on injected second file: {job}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(counts.lock().unwrap()[0], 1);
    assert_eq!(counts.lock().unwrap()[1], 1);
    failed.store(false, Ordering::SeqCst);
    let retried = client
        .post(format!("{url}/v1/transfers/{id}/retry"))
        .send()
        .await
        .unwrap();
    assert_eq!(retried.status(), StatusCode::OK);
    loop {
        let job: Value = client
            .get(format!("{url}/v1/transfers/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if job["status"] == "completed" {
            assert_eq!(job["files"][0]["bytes"], 3);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline + std::time::Duration::from_secs(8),
            "download retry did not finish: {job}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(counts.lock().unwrap()[0], 1);
    assert_eq!(counts.lock().unwrap()[1], 2);
    assert_eq!(
        std::fs::read(root.join("downloads/one.txt")).unwrap(),
        b"one"
    );
    assert_eq!(
        std::fs::read(root.join("downloads/two.txt")).unwrap(),
        b"two"
    );
    task.abort();
    let _ = task.await;
    peer_task.abort();
    let _ = peer_task.await;
    std::env::remove_var("PEERCARRY_TAILSCALE");
    let _ = std::fs::remove_dir_all(root);
}
