use std::{net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    extract::ConnectInfo,
    middleware::{self, Next},
    response::Response,
};
use peercarry_core::{config::Config, engine::Engine, model::Origin, server, store::Store};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

const NETWORK_TOKEN: &str = "synthetic-network-0123456789ABCDEF0123456789";
const MOBILE_TOKEN: &str = "synthetic-mobile-0123456789ABCDEF0123456789";
const OLD_AI_TOKEN: &str = "synthetic-legacy-ai";
const NEW_AI_TOKEN: &str = "synthetic-ai-0123456789ABCDEF0123456789";
const NEW_NETWORK_TOKEN: &str = "synthetic-new-network-0123456789ABCDEF0123456789";

// Only the test's outer layer may replace/remove connection metadata.
async fn simulate_connection(mut request: axum::http::Request<Body>, next: Next) -> Response {
    if request.headers().contains_key("x-test-peer") {
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([100, 64, 0, 23], 1000))));
    }
    if request.headers().contains_key("x-test-no-connect") {
        request.extensions_mut().remove::<ConnectInfo<SocketAddr>>();
    }
    next.run(request).await
}

async fn read_envelope(client: &Client, base: &str) -> Value {
    let response = client
        .get(format!("{base}/v1/desktop/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let body = response.text().await.unwrap();
    for secret in [
        NETWORK_TOKEN,
        MOBILE_TOKEN,
        OLD_AI_TOKEN,
        NEW_AI_TOKEN,
        NEW_NETWORK_TOKEN,
    ] {
        assert!(
            !body.contains(secret),
            "response contained a synthetic secret"
        );
    }
    serde_json::from_str(&body).unwrap()
}

async fn save(client: &Client, base: &str, body: Value) -> reqwest::Response {
    client
        .post(format!("{base}/v1/desktop/settings"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn strict_local_settings_are_redacted_versioned_and_preserve_hidden_configuration() {
    let root =
        std::env::temp_dir().join(format!("peercarry-settings-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let path = root.join("config.toml");
    let raw = format!(
        r#"
ai_token = "{OLD_AI_TOKEN}"
[node]
name = "Before"
allow_remote_apply = true
[network]
auth_token = "{NETWORK_TOKEN}"
transfer_idle_timeout_secs = 77
transfer_prepare_timeout_secs = 888
[mobile]
enabled = true
[[mobile.devices]]
id = "synthetic-phone"
token = "{MOBILE_TOKEN}"
[custom_section]
unknown_setting = "keep-this"
"#
    );
    let mut config: Config = toml::from_str(&raw).unwrap();
    config.mobile.validate().unwrap();
    config.storage.download_dir = Some(root.join("initial-downloads"));
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let origin = Origin {
        host: "test".into(),
        node_id: "test".into(),
        addr: base.clone(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = server::router(server::AppState {
        store,
        config,
        origin,
        engine: engine.clone(),
    })
    .layer(middleware::from_fn(simulate_connection));
    let server_task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();

    let initial = read_envelope(&client, &base).await;
    // Following a changed-port link may navigate across origins. Only a
    // user-activated top-level HTML GET gains this path, never an API.
    for site in ["same-site", "cross-site"] {
        for target in ["/", "/v1/desktop/settings", "/v1/config", "/v1/notebook"] {
            let response = client
                .get(format!("{base}{target}"))
                .header("sec-fetch-site", site)
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-user", "?1")
                .send()
                .await
                .unwrap();
            if target == "/" {
                assert_eq!(response.status(), StatusCode::OK);
                assert!(response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html"));
            } else {
                assert!(matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ));
            }
        }
    }
    assert!(!path.exists(), "GET must not create the configuration file");
    assert_eq!(initial["schema"], 1);
    uuid::Uuid::parse_str(initial["instance_id"].as_str().unwrap()).unwrap();
    assert_eq!(initial["restart_recommended"], false);
    assert_eq!(initial["desktop_url"], "http://127.0.0.1:5199/");
    assert_eq!(initial["secrets"]["network.auth_token"], true);
    assert_eq!(initial["secrets"]["ai_token"], true);
    assert_eq!(initial["values"]["node.name"], "Before");
    assert!(initial["values"].get("mobile").is_none());
    assert!(initial["values"]
        .get("network.transfer_idle_timeout_secs")
        .is_none());
    assert!(initial["values"]
        .get("network.transfer_prepare_timeout_secs")
        .is_none());
    assert!(initial["values"].get("network.auth_token").is_none());
    assert!(initial["values"].get("ai_token").is_none());
    let expected_fields: std::collections::BTreeSet<_> = [
        "node.name",
        "node.discoverable",
        "node.allow_remote_apply",
        "node.allow_remote_capture",
        "node.allow_remote_launch",
        "network.port",
        "network.bind",
        "network.request_timeout_secs",
        "network.connect_timeout_secs",
        "network.discovery_concurrency",
        "limits.inline_image_bytes",
        "limits.inline_text_bytes",
        "limits.max_transfer_bytes",
        "storage.history_limit",
        "storage.thumbnail_px",
        "storage.dedupe",
        "storage.download_dir",
        "storage.data_dir",
        "updates.auto_check",
        "updates.check_interval_hours",
        "updates.source",
        "updates.domestic_url",
        "updates.github_url",
        "updates.custom_url",
        "updates.public_key",
        "apps",
    ]
    .into_iter()
    .collect();
    let actual_fields: std::collections::BTreeSet<_> = initial["values"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual_fields, expected_fields);

    let first_save = save(
        &client,
        &base,
        json!({"revision":initial["revision"],"changes":{}}),
    )
    .await;
    assert_eq!(first_save.status(), StatusCode::OK);
    assert!(
        path.is_file(),
        "saving a missing configuration establishes the startup baseline"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let first_file = std::fs::read_to_string(&path).unwrap();
    assert!(first_file.contains(NETWORK_TOKEN));
    assert!(first_file.contains(MOBILE_TOKEN));

    // Peer authentication and allow_remote_apply do not grant these routes.
    for path_suffix in ["/v1/desktop/settings", "/v1/desktop/settings/restart"] {
        for marker in ["x-test-peer", "x-test-no-connect"] {
            let response = client
                .get(format!("{base}{path_suffix}"))
                .header(marker, "1")
                .header("x-syncclip-token", NETWORK_TOKEN)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        }
        let response = client
            .post(format!("{base}{path_suffix}"))
            .header("x-test-peer", "1")
            .header("x-syncclip-token", NETWORK_TOKEN)
            .json(&json!({"revision":"synthetic"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for (header, value) in [
        ("host", "untrusted.example"),
        ("origin", "https://untrusted.example"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let response = client
            .get(format!("{base}/v1/desktop/settings"))
            .header(header, value)
            .header("x-syncclip-token", NETWORK_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    }
    let duplicate_origin = client
        .get(format!("{base}/v1/desktop/settings"))
        .header("origin", &base)
        .header("origin", "https://untrusted.example")
        .header("x-syncclip-token", NETWORK_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate_origin.status(), StatusCode::FORBIDDEN);
    let old_api = client
        .get(format!("{base}/v1/config"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_api.status(), StatusCode::UNAUTHORIZED);
    let old_api_authorized = client
        .get(format!("{base}/v1/config"))
        .header("x-syncclip-token", NETWORK_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(old_api_authorized.status(), StatusCode::OK);

    std::fs::write(&path, raw.as_bytes()).unwrap();
    let before = read_envelope(&client, &base).await;
    let revision = before["revision"].as_str().unwrap().to_owned();
    assert_ne!(initial["revision"], revision);
    let original_file = std::fs::read(&path).unwrap();
    let original_download_dir = engine.download_dir();
    for (case, invalid) in [
        json!({"revision":revision,"changes":{"node.name":true}}),
        json!({"revision":revision,"changes":{"network.auth_token":"should-not-be-saved"}}),
        json!({"revision":revision,"changes":{"mobile.enabled":false}}),
        json!({"revision":revision,"changes":{},"unknown":1}),
        json!({"revision":revision,"secret_changes":{"ai_token":{"op":"replace","value":"short-secret"}}}),
        json!({"revision":revision,"secret_changes":{"ai_token":{"op":"clear","value":"extra"}}}),
        json!({"revision":revision,"secret_changes":{"mobile.token":{"op":"clear"}}}),
        json!({"revision":revision,"changes":{"apps":[{"name":"x","path":"x","unknown":"x"}]}}),
    ].into_iter().enumerate() {
        let response = save(&client, &base, invalid).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "invalid case {case}");
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        let error = response.text().await.unwrap();
        assert!(!error.contains("short-secret"));
        assert!(!error.contains(NETWORK_TOKEN));
        assert_eq!(std::fs::read(&path).unwrap(), original_file);
        assert_eq!(engine.download_dir(), original_download_dir);
    }
    let duplicate = format!(
        r#"{{"revision":"{revision}","changes":{{"node.name":"first","node.name":"second"}}}}"#
    );
    let duplicate = client
        .post(format!("{base}/v1/desktop/settings"))
        .header("content-type", "application/json")
        .body(duplicate)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read(&path).unwrap(), original_file);
    let too_large = client
        .post(format!("{base}/v1/desktop/settings"))
        .header("content-type", "application/json")
        .body("x".repeat(256 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        too_large.headers().get("cache-control").unwrap(),
        "no-store"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original_file);

    let download = root.join("synthetic-downloads");
    let response = save(
        &client,
        &base,
        json!({"revision":revision,"changes":{
            "node.name":" After ","storage.download_dir":download.to_str().unwrap()
        }}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["values"]["node.name"], "After");
    assert_eq!(saved["restart_recommended"], true);
    assert_eq!(saved["instance_id"], initial["instance_id"]);
    let observed = read_envelope(&client, &base).await;
    assert_eq!(observed["instance_id"], initial["instance_id"]);
    assert_eq!(observed["restart_recommended"], true);
    assert_eq!(engine.download_dir(), download);
    let saved_file = std::fs::read_to_string(&path).unwrap();
    let saved_doc: toml::Value = toml::from_str(&saved_file).unwrap();
    assert_eq!(
        saved_doc["mobile"]["devices"][0]["token"].as_str(),
        Some(MOBILE_TOKEN)
    );
    assert_eq!(
        saved_doc["network"]["auth_token"].as_str(),
        Some(NETWORK_TOKEN)
    );
    assert_eq!(saved_doc["ai_token"].as_str(), Some(OLD_AI_TOKEN));
    assert_eq!(
        saved_doc["network"]["transfer_idle_timeout_secs"].as_integer(),
        Some(77)
    );
    assert_eq!(
        saved_doc["network"]["transfer_prepare_timeout_secs"].as_integer(),
        Some(888)
    );
    assert_eq!(
        saved_doc["custom_section"]["unknown_setting"].as_str(),
        Some("keep-this")
    );

    // Old page cannot overwrite the first successful save.
    let response = save(
        &client,
        &base,
        json!({"revision":revision,"changes":{"node.name":"stale"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "settings_conflict"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved_file);

    let revision2 = saved["revision"].as_str().unwrap();
    let response = save(&client, &base, json!({"revision":revision2,"changes":{"node.discoverable":false},
        "secret_changes":{"network.auth_token":{"op":"replace","value":NEW_NETWORK_TOKEN},"ai_token":{"op":"replace","value":NEW_AI_TOKEN}}})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(
        !body.contains(NEW_NETWORK_TOKEN)
            && !body.contains(NEW_AI_TOKEN)
            && !body.contains(MOBILE_TOKEN)
    );
    let changed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(changed["secrets"]["network.auth_token"], true);
    assert_eq!(changed["secrets"]["ai_token"], true);
    let changed_file = std::fs::read_to_string(&path).unwrap();
    let changed_doc: toml::Value = toml::from_str(&changed_file).unwrap();
    assert_eq!(
        changed_doc["network"]["auth_token"].as_str(),
        Some(NEW_NETWORK_TOKEN)
    );
    assert_eq!(changed_doc["ai_token"].as_str(), Some(NEW_AI_TOKEN));
    assert_eq!(
        changed_doc["mobile"]["devices"][0]["token"].as_str(),
        Some(MOBILE_TOKEN)
    );

    let response = save(
        &client,
        &base,
        json!({"revision":changed["revision"],"changes":{},
        "secret_changes":{"ai_token":{"op":"clear"}}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cleared: Value = response.json().await.unwrap();
    assert_eq!(cleared["secrets"]["ai_token"], false);
    assert_eq!(cleared["secrets"]["network.auth_token"], true);
    let cleared_file = std::fs::read_to_string(&path).unwrap();
    let cleared_doc: toml::Value = toml::from_str(&cleared_file).unwrap();
    assert!(cleared_doc.get("ai_token").is_none());
    assert_eq!(
        cleared_doc["network"]["auth_token"].as_str(),
        Some(NEW_NETWORK_TOKEN)
    );
    assert_eq!(
        cleared_doc["mobile"]["devices"][0]["token"].as_str(),
        Some(MOBILE_TOKEN)
    );

    // External edits invalidate both save and restart without touching the file.
    let externally_changed = format!("{cleared_file}\n[external_section]\nvalue = 7\n");
    std::fs::write(&path, &externally_changed).unwrap();
    let response = save(
        &client,
        &base,
        json!({"revision":cleared["revision"],"changes":{"node.name":"outdated"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let stale_restart = client
        .post(format!("{base}/v1/desktop/settings/restart"))
        .json(&json!({"revision":cleared["revision"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_restart.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), externally_changed);

    let current = read_envelope(&client, &base).await;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let before_security = windows_security(&path);
        // Permit reads, but disallow the delete share needed for replacement.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let attempt = save(&client, &base, json!({"revision":current["revision"],"changes":{"storage.download_dir":root.join("must-not-apply").to_str().unwrap()}})).await;
        assert_eq!(attempt.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), externally_changed);
        assert_eq!(engine.download_dir(), download);
        drop(lock);
        let successful = save(
            &client,
            &base,
            json!({"revision":current["revision"],"changes":{"node.name":"ACL test"}}),
        )
        .await;
        assert_eq!(successful.status(), StatusCode::OK);
        assert_eq!(windows_security(&path), before_security);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let successful = save(
            &client,
            &base,
            json!({"revision":current["revision"],"changes":{"node.name":"mode test"}}),
        )
        .await;
        assert_eq!(successful.status(), StatusCode::OK);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    let concurrent_base = read_envelope(&client, &base).await;
    let (first, second) = tokio::join!(
        save(
            &client,
            &base,
            json!({"revision":concurrent_base["revision"],"changes":{"node.name":"first"}})
        ),
        save(
            &client,
            &base,
            json!({"revision":concurrent_base["revision"],"changes":{"node.name":"second"}})
        ),
    );
    let statuses = [first.status(), second.status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::CONFLICT));

    let before_apps = read_envelope(&client, &base).await;
    let response = save(
        &client,
        &base,
        json!({"revision":before_apps["revision"],"changes":{
            "apps":[{"name":" Synthetic App ","path":"synthetic.exe","args":["--safe"]}]
        }}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let apps: Value = response.json().await.unwrap();
    assert_eq!(apps["values"]["apps"][0]["name"], "Synthetic App");
    assert_eq!(apps["values"]["apps"][0]["args"][0], "--safe");

    let before_clear_network = read_envelope(&client, &base).await;
    let response = save(
        &client,
        &base,
        json!({"revision":before_clear_network["revision"],"changes":{},
        "secret_changes":{"network.auth_token":{"op":"clear"}}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let network_cleared: Value = response.json().await.unwrap();
    assert_eq!(network_cleared["secrets"]["network.auth_token"], false);
    assert_eq!(network_cleared["secrets"]["ai_token"], false);
    let final_doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(final_doc["network"].get("auth_token").is_none());
    assert_eq!(
        final_doc["mobile"]["devices"][0]["token"].as_str(),
        Some(MOBILE_TOKEN)
    );

    let last = read_envelope(&client, &base).await;
    std::fs::write(&path, "not = [ valid TOML\n").unwrap();
    let malformed = client
        .get(format!("{base}/v1/desktop/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error_body = malformed.text().await.unwrap();
    assert!(!error_body.contains("not ="));
    let failed_save = save(
        &client,
        &base,
        json!({"revision":last["revision"],"changes":{"node.name":"cannot overwrite"}}),
    )
    .await;
    assert_eq!(failed_save.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "not = [ valid TOML\n"
    );
    server_task.abort();
}

#[cfg(windows)]
fn windows_security(path: &std::path::Path) -> String {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[link(name = "advapi32")]
    extern "system" {
        fn GetFileSecurityW(
            name: *const u16,
            requested: u32,
            descriptor: *mut c_void,
            length: u32,
            needed: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *const c_void,
            revision: u32,
            requested: u32,
            result: *mut *mut u16,
            length: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    const SECURITY: u32 = 0x1 | 0x2 | 0x4 | 0x10;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut needed = 0u32;
    unsafe {
        GetFileSecurityW(
            name.as_ptr(),
            SECURITY,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    assert!(needed > 0);
    let mut descriptor = vec![0u8; needed as usize];
    assert_ne!(
        unsafe {
            GetFileSecurityW(
                name.as_ptr(),
                SECURITY,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        },
        0
    );
    let mut result = std::ptr::null_mut();
    let mut length = 0u32;
    assert_ne!(
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor.as_ptr().cast(),
                1,
                SECURITY,
                &mut result,
                &mut length,
            )
        },
        0
    );
    let text = String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(result, length.saturating_sub(1) as usize)
    });
    unsafe {
        LocalFree(result.cast());
    }
    text
}
