//! Black-box HTTP acceptance checks for the mobile upload surface.
//!
//! The single test keeps `PEERCARRY_DATA_DIR` and the redb store isolated from
//! parallel tests.  It intentionally exercises the assembled server router.

use std::sync::Arc;
use std::time::Duration;

use axum::serve;
use peercarry_core::config::{Config, MobileDevice};
use peercarry_core::engine::Engine;
use peercarry_core::model::Origin;
use peercarry_core::paths;
use peercarry_core::server::{self, AppState};
use peercarry_core::store::Store;
use reqwest::{Client, StatusCode};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEVICE: &str = "phone-test";
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn start(config: Config) -> (String, tokio::task::JoinHandle<()>) {
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&paths::db_path()).expect("test store"));
    let origin = Origin {
        host: "test".into(),
        node_id: "test-node".into(),
        addr: "http://127.0.0.1".into(),
        os: "test".into(),
    };
    let engine =
        Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).expect("test engine"));
    let app = server::router(AppState {
        store,
        config,
        origin,
        engine,
    });
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("listener");
    let addr = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        serve(listener, app).await.expect("server");
    });
    (format!("http://{addr}"), task)
}

fn config(enabled: bool) -> Config {
    let mut c = Config::default();
    c.mobile.enabled = enabled;
    c.mobile.devices = vec![MobileDevice {
        id: DEVICE.into(),
        token: TOKEN.into(),
    }];
    c.mobile.max_storage_bytes = 3;
    c.mobile.max_sessions = 4;
    // Production timestamps have one-second precision; leave enough time for
    // consecutive requests even when the test crosses a wall-clock boundary.
    c.mobile.session_ttl_secs = 3;
    c.limits.max_transfer_bytes = 2;
    c
}

fn auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("x-syncclip-device-id", DEVICE)
        .header("x-syncclip-device-token", TOKEN)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_upload_http_limits_and_lifecycle() {
    let root = std::env::temp_dir().join(format!("peercarry-mobile-test-{}", Uuid::new_v4()));
    let disabled_root = root.join("disabled");
    let enabled_root = root.join("enabled");
    std::fs::create_dir_all(&disabled_root).expect("root");
    std::env::set_var("PEERCARRY_DATA_DIR", &disabled_root);
    let client = Client::new();

    let (base, disabled_task) = start(config(false)).await;
    let req = client
        .post(format!("{base}/v1/uploads"))
        .json(&serde_json::json!({
            "request_id": Uuid::new_v4().to_string(), "filename": "a.txt", "size": 1,
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }));
    assert_eq!(
        auth(req).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        auth(client.get(format!("{base}/v1/uploads/{}", Uuid::new_v4())))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let disabled_id = Uuid::new_v4();
    for method in ["PATCH", "DELETE", "POST"] {
        let url = if method == "POST" {
            format!("{base}/v1/uploads/{disabled_id}/complete")
        } else {
            format!("{base}/v1/uploads/{disabled_id}")
        };
        let req = match method {
            "PATCH" => client.patch(url).header("Upload-Offset", "0").body("x"),
            "DELETE" => client.delete(url),
            _ => client.post(url),
        };
        assert_eq!(
            auth(req).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
    disabled_task.abort();

    std::fs::create_dir_all(&enabled_root).expect("enabled root");
    std::env::set_var("PEERCARRY_DATA_DIR", &enabled_root);
    let (base, task) = start(config(true)).await;
    assert_eq!(
        client
            .get(format!("{base}/v1/uploads/{}", Uuid::new_v4()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let valid_sha = peercarry_core::model::sha256_hex(b"x");
    for filename in ["../evil", "CON", "trailing "] {
        let req = client
            .post(format!("{base}/v1/uploads"))
            .json(&serde_json::json!({
                "request_id": Uuid::new_v4().to_string(), "filename": filename, "size": 1,
                "sha256": valid_sha
            }));
        let status = auth(req).send().await.unwrap().status();
        assert!(status.is_client_error());
    }

    // A user supplied name matching an internal file is safe: the published
    // payload is stored at a server controlled path and retains this name in
    // the Entry/FileRef metadata.
    let id = Uuid::new_v4();
    let create = auth(
        client
            .post(format!("{base}/v1/uploads"))
            .json(&serde_json::json!({
                "request_id": id.to_string(), "filename": "payload.part", "size": 1, "sha256": valid_sha
            })),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body: serde_json::Value = create.json().await.unwrap();
    let upload_id = body["id"].as_str().unwrap();
    let req = auth(
        client
            .patch(format!("{base}/v1/uploads/{upload_id}"))
            .header("Upload-Offset", "1")
            .body("o"),
    );
    assert_eq!(req.send().await.unwrap().status(), StatusCode::CONFLICT);
    let req = auth(
        client
            .patch(format!("{base}/v1/uploads/{upload_id}"))
            .header("Upload-Offset", "0")
            .body("x"),
    );
    assert_eq!(req.send().await.unwrap().status(), StatusCode::OK);
    let complete = auth(client.post(format!("{base}/v1/uploads/{upload_id}/complete")))
        .send()
        .await
        .unwrap();
    assert_eq!(complete.status(), StatusCode::OK);
    let complete_body: serde_json::Value = complete.json().await.unwrap();
    let entry_id = complete_body["entry_id"].as_str().unwrap();
    assert_eq!(
        auth(client.get(format!("{base}/v1/entries/{entry_id}")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        auth(client.post(format!("{base}/v1/uploads/{upload_id}/complete")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // A wrong digest is rejected and does not create a history entry.
    let bad_id = Uuid::new_v4();
    let bad_create = auth(
        client
            .post(format!("{base}/v1/uploads"))
            .json(&serde_json::json!({
                "request_id": bad_id.to_string(), "filename": "bad.bin", "size": 1,
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
            })),
    )
    .send()
    .await
    .unwrap();
    let bad_upload: serde_json::Value = bad_create.json().await.unwrap();
    let bad_upload_id = bad_upload["id"].as_str().unwrap();
    assert_eq!(
        auth(
            client
                .patch(format!("{base}/v1/uploads/{bad_upload_id}"))
                .header("Upload-Offset", "0")
                .body("x")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        auth(client.post(format!("{base}/v1/uploads/{bad_upload_id}/complete")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        auth(client.get(format!("{base}/v1/entries/{bad_upload_id}")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        auth(client.delete(format!("{base}/v1/uploads/{bad_upload_id}")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );

    // The completed one-byte upload plus this two-byte incomplete session
    // fills the three-byte reservation quota.
    let held = auth(
        client
            .post(format!("{base}/v1/uploads"))
            .json(&serde_json::json!({
                "request_id": Uuid::new_v4().to_string(), "filename": "held", "size": 2,
                "sha256": valid_sha
            })),
    );
    assert_eq!(held.send().await.unwrap().status(), StatusCode::CREATED);
    let quota_full = auth(client.post(format!("{base}/v1/uploads")).json(&serde_json::json!({
        "request_id": Uuid::new_v4().to_string(), "filename": "full", "size": 1, "sha256": valid_sha
    })));
    assert!(quota_full.send().await.unwrap().status().is_client_error());

    tokio::time::sleep(Duration::from_millis(3100)).await;
    let after_ttl = auth(client.post(format!("{base}/v1/uploads")).json(&serde_json::json!({
        "request_id": Uuid::new_v4().to_string(), "filename": "after-ttl", "size": 2, "sha256": valid_sha
    })));
    assert_eq!(
        after_ttl.send().await.unwrap().status(),
        StatusCode::CREATED
    );
    let completed_still_counts = auth(client.post(format!("{base}/v1/uploads")).json(&serde_json::json!({
        "request_id": Uuid::new_v4().to_string(), "filename": "still-full", "size": 1, "sha256": valid_sha
    })));
    assert!(completed_still_counts
        .send()
        .await
        .unwrap()
        .status()
        .is_client_error());
    task.abort();
    let _ = std::fs::remove_dir_all(root);
}
