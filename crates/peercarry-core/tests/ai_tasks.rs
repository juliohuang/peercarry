use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use peercarry_core::ai_tasks::{drain_outbox, ingest_event, list_events, AiEventInput};
use peercarry_core::{config::Config, engine::Engine, model::Origin, server, store::Store};
use reqwest::{Client, StatusCode};
use serde_json::json;
use uuid::Uuid;

fn temp_db() -> PathBuf {
    std::env::temp_dir().join(format!("peercarry-ai-test-{}.redb", Uuid::new_v4()))
}

fn input(id: Uuid) -> AiEventInput {
    AiEventInput {
        id: id.to_string(),
        tool: "codex".into(),
        session_id: "session-1".into(),
        project: Some("C:/work/example/project".into()),
        hook: "Stop".into(),
    }
}

#[test]
fn ingest_is_metadata_only_and_deduplicated() {
    let path = temp_db();
    let id = Uuid::new_v4();
    let first = ingest_event(&path, input(id), "node-a").unwrap();
    let duplicate = ingest_event(&path, input(id), "node-b").unwrap();
    assert_eq!(first, duplicate);
    assert_eq!(first.project.as_deref(), Some("project"));
    assert_eq!(first.sequence, 1);
    assert_eq!(list_events(&path).unwrap().len(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn invalid_tool_and_hook_are_rejected() {
    let path = temp_db();
    let mut bad = input(Uuid::new_v4());
    bad.tool = "other".into();
    assert!(ingest_event(&path, bad, "node").is_err());
    let mut bad_hook = input(Uuid::new_v4());
    bad_hook.hook = "UserPromptSubmit\nsecret prompt".into();
    assert!(ingest_event(&path, bad_hook, "node").is_err());
    let _ = std::fs::remove_file(path);
}

async fn override_connect_info(mut request: Request<Body>, next: Next) -> Response {
    if let Some(value) = request.headers().get("x-test-remote") {
        if let Ok(addr) = value.to_str().unwrap_or_default().parse::<SocketAddr>() {
            request.extensions_mut().insert(ConnectInfo(addr));
        }
    }
    next.run(request).await
}

#[tokio::test]
async fn http_gates_dedup_and_outbox_replay() {
    let root = std::env::temp_dir().join(format!("peercarry-ai-http-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut config = Config::default();
    config.network.auth_token = Some("test-shared-token".into());
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "http-test".into(),
        node_id: "test".into(),
        addr: url.clone(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = server::router(server::AppState {
        config,
        store,
        origin,
        engine,
    })
    .layer(middleware::from_fn(override_connect_info));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let endpoint = format!("{url}/v1/ai/events");
    let id = Uuid::new_v4().to_string();
    let event =
        json!({"id": id, "tool":"codex", "session_id":"s1", "project":"C:/work/p", "hook":"Stop"});
    let token = "test-shared-token";

    let remote_post = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .header("x-syncclip-ai-hook", "1")
        .header("x-test-remote", "100.64.0.2:1")
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(remote_post.status(), StatusCode::FORBIDDEN);
    let browser_post = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .header("x-syncclip-ai-hook", "1")
        .header("origin", "https://example.test")
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(browser_post.status(), StatusCode::FORBIDDEN);
    let missing_hook = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_hook.status(), StatusCode::FORBIDDEN);
    let malformed = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .header("x-syncclip-ai-hook", "1")
        .json(&json!({"id":"bad"}))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let created = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .header("x-syncclip-ai-hook", "1")
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let duplicate = client
        .post(&endpoint)
        .header("x-syncclip-token", token)
        .header("x-syncclip-ai-hook", "1")
        .json(&event)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CREATED);
    assert_eq!(
        created.json::<serde_json::Value>().await.unwrap()["id"],
        duplicate.json::<serde_json::Value>().await.unwrap()["id"]
    );

    let no_token = client
        .get(format!("{url}/v1/ai/events"))
        .header("x-test-remote", "100.64.0.2:1")
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);
    let shared = client
        .get(format!("{url}/v1/ai/events"))
        .header("x-syncclip-token", token)
        .header("x-test-remote", "100.64.0.2:1")
        .send()
        .await
        .unwrap();
    assert_eq!(shared.status(), StatusCode::OK);
    assert_eq!(
        shared.json::<Vec<serde_json::Value>>().await.unwrap().len(),
        1
    );

    let outbox = root.join("ai-outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    let replay_id = Uuid::new_v4().to_string();
    std::fs::write(outbox.join("replay.json"), serde_json::to_vec(&json!({"id":replay_id,"tool":"zcode","session_id":"s2","project":"/tmp/replay","hook":"SessionEnd"})).unwrap()).unwrap();
    assert_eq!(drain_outbox(&root, "http-test").unwrap(), 1);
    assert_eq!(drain_outbox(&root, "http-test").unwrap(), 0);
    assert_eq!(list_events(&root.join("ai-events.redb")).unwrap().len(), 2);
    task.abort();
    let _ = std::fs::remove_dir_all(root);
}
